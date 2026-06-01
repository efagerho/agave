//! Caller-driven addr refresh: when a caller (e.g. votor via a gossip
//! refresh) hands the endpoint a new SocketAddr for a pubkey already in
//! the connection table, the lex-lower (dialer) side must evict its
//! stale outbound connection and re-dial the new addr. Lex-higher
//! cached entries are server-accepted (peer's NAT-mapped source addr
//! can legitimately differ from gossip's published one) and are trusted
//! regardless of the caller's claimed addr.

#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_keypair::Keypair,
    solana_net_utils::sockets::bind_to_localhost_unique,
    solana_quic_datagram::{admission::AllowAll, endpoint::Datagram},
    std::{sync::Arc, time::Duration},
};

fn clone_keypair(k: &Keypair) -> Keypair {
    k.insecure_clone()
}

#[test]
fn outbound_addr_change_redials_new_addr() {
    let rt = make_runtime();

    // S1 establishes the lex order. Client will be lex-lower.
    let s1 = spawn_node(&rt, Arc::new(AllowAll));
    let s_key = clone_keypair(&s1.keypair);
    let s_pubkey = s1.pubkey();

    let client_kp = keypair_below(&s_pubkey);
    let client = spawn_node_with(&rt, Arc::new(AllowAll), client_kp);

    // Initial send: client dials S1 at its addr A1.
    let p1 = Bytes::from_static(b"p1");
    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        s1.addr,
        p1.clone(),
        &s1.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p1).then_some(()),
        "S1 did not receive p1",
    );
    drain_matching(&s1.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p1
    });

    // S2 takes the same identity at a new addr - simulates a peer host
    // move with gossip publishing a new SocketAddr for the same pubkey.
    let s2 = spawn_node_with(&rt, Arc::new(AllowAll), s_key);
    assert_ne!(s1.addr, s2.addr, "S1 and S2 must bind distinct addrs");

    // Send to the new addr. Client must observe the addr mismatch,
    // evict its cached conn to A1, and re-dial A2.
    let p2 = Bytes::from_static(b"p2");
    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        s2.addr,
        p2.clone(),
        &s2.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p2).then_some(()),
        "S2 (post-move) did not receive p2",
    );

    // Stale conn to S1 was evicted; S1 must not see post-move datagrams.
    let stray = s1.ingress_rx.recv_timeout(Duration::from_millis(800));
    assert!(
        stray.is_err(),
        "S1 must not see post-move datagrams; got {stray:?}"
    );
}

#[test]
fn higher_side_ignores_caller_addr_on_hit() {
    // Inbound (server-accepted) connections live on the lex-higher side.
    // Their cached `remote_address` is the peer's NAT-mapped source addr,
    // which the caller (working from gossip) may not know. The higher
    // side must trust the cached conn regardless of caller's addr.
    let rt = make_runtime();

    let server = spawn_node(&rt, Arc::new(AllowAll)); // lex-higher
    let client_kp = keypair_below(&server.pubkey());
    let client = spawn_node_with(&rt, Arc::new(AllowAll), client_kp);
    let c_pubkey = client.pubkey();

    // Client dials in so the server caches an inbound conn for c_pubkey.
    let probe = Bytes::from_static(b"open");
    send_until_received(
        &rt,
        &client.endpoint,
        server.pubkey(),
        server.addr,
        probe.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == probe).then_some(()),
        "server did not receive client's probe",
    );

    // Server now sends back to client. The server is lex-higher and
    // already holds an `Established` for c_pubkey (the inbound from
    // the client's dial), so `send_over_inbound` goes through the cached
    // connection without a dial — single send + recv works here.
    // We deliberately hand a bogus addr to verify the higher side
    // ignores it on cache hit.
    let bogus_addr: std::net::SocketAddr = "203.0.113.99:65000".parse().unwrap();
    let pay = Bytes::from_static(b"reply");
    rt.block_on(async {
        server
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: c_pubkey,
                peer_address: bogus_addr,
                message: pay.clone(),
            })
            .await
            .unwrap();
    });
    common::recv_until(
        &client.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == pay).then_some(()),
        "client did not receive server's reply on the cached inbound conn",
    );
}

#[test]
fn egress_during_dial_dropped_addr_recovers_after_timeout() {
    // Mid-dial address change: while a dial to addr A1 is still in flight
    // (blackhole'd, will time out), the caller queues an egress with a
    // different addr A2 for the same pubkey. The state-machine drops it
    // (placeholder is `Dialing`, not `Established`, so the addr-mismatch
    // eviction path doesn't apply). Once the first dial times out and
    // clears the placeholder, a fresh egress to A2 must succeed.
    let rt = make_runtime();
    let server = spawn_node(&rt, Arc::new(AllowAll));
    let s_pubkey = server.pubkey();
    let client = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&s_pubkey));

    let blackhole = bind_to_localhost_unique().expect("blackhole socket");
    let blackhole_addr = blackhole.local_addr().expect("blackhole addr");

    // 1. start dial to blackhole - server's pubkey, wrong addr.
    let p1 = Bytes::from_static(b"dial-to-blackhole");
    rt.block_on(async {
        client
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: s_pubkey,
                peer_address: blackhole_addr,
                message: p1.clone(),
            })
            .await
            .unwrap();
    });
    // Give the dial a beat to install its `Dialing` placeholder.
    std::thread::sleep(Duration::from_millis(200));

    // 2. while `Dialing`, send to the REAL addr. Should be dropped (we do
    // not buffer; one dial task per peer at a time).
    let p2 = Bytes::from_static(b"during-dial-dropped");
    rt.block_on(async {
        client
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: s_pubkey,
                peer_address: server.addr,
                message: p2.clone(),
            })
            .await
            .unwrap();
    });
    // Confirm server does NOT see p2 within the dial-in-progress window.
    let stray = server.ingress_rx.recv_timeout(Duration::from_millis(500));
    assert!(
        stray.is_err(),
        "server unexpectedly received {stray:?} while dial-in-flight"
    );

    // 3. wait for the blackhole dial to time out, clearing the placeholder.
    std::thread::sleep(Duration::from_secs(8));

    // 4. retry the real addr - must succeed.
    let p3 = Bytes::from_static(b"post-timeout-retry");
    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        server.addr,
        p3.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p3).then_some(()),
        "server did not receive post-timeout retry",
    );

    // p1 and p2 never reach the server: p1 went to the blackhole, p2 was
    // dropped while `Dialing`. Drain p3 retry duplicates, then assert
    // no foreign packet arrives.
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p3
    });
    let stray = server.ingress_rx.recv_timeout(Duration::from_millis(500));
    assert!(
        stray.is_err(),
        "server unexpectedly received {stray:?} after the retry landed",
    );
    drop(blackhole);
}

#[test]
fn chained_address_changes_each_redial() {
    // Three servers sharing one identity, distinct addrs. Each egress to
    // a new addr evicts the prior `Established` with PEER_MOVED and dials
    // the new addr. Verifies the state machine can roll through
    // Established(A1) → Dialing → Established(A2) → Dialing →
    // Established(A3) without corruption.
    let rt = make_runtime();
    let s1 = spawn_node(&rt, Arc::new(AllowAll));
    let s_key = clone_keypair(&s1.keypair);
    let s_pubkey = s1.pubkey();
    let client = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&s_pubkey));

    let s2 = spawn_node_with(&rt, Arc::new(AllowAll), clone_keypair(&s_key));
    let s3 = spawn_node_with(&rt, Arc::new(AllowAll), clone_keypair(&s_key));
    assert!(
        s1.addr != s2.addr && s2.addr != s3.addr && s1.addr != s3.addr,
        "the three servers must bind distinct addrs",
    );

    let p1 = Bytes::from_static(b"to-s1");
    let p2 = Bytes::from_static(b"to-s2");
    let p3 = Bytes::from_static(b"to-s3");

    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        s1.addr,
        p1.clone(),
        &s1.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p1).then_some(()),
        "s1 did not receive p1",
    );
    drain_matching(&s1.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p1
    });

    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        s2.addr,
        p2.clone(),
        &s2.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p2).then_some(()),
        "s2 did not receive p2 - first address change failed",
    );
    drain_matching(&s2.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p2
    });

    send_until_received(
        &rt,
        &client.endpoint,
        s_pubkey,
        s3.addr,
        p3.clone(),
        &s3.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p3).then_some(()),
        "s3 did not receive p3 - chained address change failed",
    );

    // Each server saw only its targeted datagram (plus retry duplicates
    // we already drained), nothing else.
    let stray1 = s1.ingress_rx.recv_timeout(Duration::from_millis(500));
    assert!(
        stray1.is_err(),
        "s1 unexpectedly received extra {stray1:?} after move",
    );
    let stray2 = s2.ingress_rx.recv_timeout(Duration::from_millis(500));
    assert!(
        stray2.is_err(),
        "s2 unexpectedly received extra {stray2:?} after move",
    );
}
