//! HANDOVER: a new successful handshake from a pubkey already
//! in the table closes the prior connection with HANDOVER and installs the
//! new one. The displaced side soft-bans the evicting peer and forwards the
//! event to its handover_events channel.

#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, recv_until, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_keypair::{Keypair, Signer},
    solana_quic_datagram::{admission::AllowAll, endpoint::Datagram},
    std::{sync::Arc, time::Duration},
};

fn clone_keypair(k: &Keypair) -> Keypair {
    k.insecure_clone()
}

#[test]
fn second_connection_with_same_keypair_handovers_first() {
    let rt = make_runtime();
    let server = spawn_node(&rt, Arc::new(AllowAll));

    // Lex rule: shared client keypair must be lower than server's.
    let shared = keypair_below(&server.pubkey());
    let c_pubkey = shared.pubkey();
    let mut c1 = spawn_node_with(&rt, Arc::new(AllowAll), clone_keypair(&shared));

    // C1 establishes a connection by driving send-until-receive.
    let p1 = Bytes::from_static(b"p1");
    send_until_received(
        &rt,
        &c1.endpoint,
        server.pubkey(),
        server.addr,
        p1.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p1).then_some(()),
        "server did not receive c1's probe",
    );
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p1
    });

    // C2 arrives with the same keypair. Its handshake completion at the
    // server triggers HANDOVER on c1's connection.
    let c2 = spawn_node_with(&rt, Arc::new(AllowAll), clone_keypair(&shared));
    let p2 = Bytes::from_static(b"p2");
    send_until_received(
        &rt,
        &c2.endpoint,
        server.pubkey(),
        server.addr,
        p2.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == p2).then_some(()),
        "server did not receive c2's probe",
    );
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p2
    });

    let server_pk = server.pubkey();

    // C1's read loop observes the HANDOVER close: soft-bans the server and
    // forwards the server's pubkey on the handover_events channel.
    let evictor = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), c1.endpoint.handover_events.recv())
            .await
            .expect("timed out waiting for handover_events to fire")
            .expect("handover_events channel closed unexpectedly")
    });
    assert_eq!(
        evictor, server_pk,
        "handover_events must report the evicting peer's pubkey"
    );
    assert!(
        c1.banlist.is_banned(&server_pk),
        "c1 must soft-ban the server that handovered it"
    );

    // After HANDOVER the server's table holds c2 only. Server's egress to
    // c_pubkey reaches c2; c1 must not see post-handover datagrams.
    let after = Bytes::from_static(b"post-handover");
    rt.block_on(async {
        server
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: c_pubkey,
                peer_address: c1.addr,
                message: after.clone(),
            })
            .await
            .unwrap();
    });
    recv_until(
        &c2.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == after).then_some(()),
        "c2 (post-handover) must receive server's datagram",
    );
    let stray = c1.ingress_rx.recv_timeout(Duration::from_millis(800));
    assert!(
        stray.is_err(),
        "c1 was handovered; must not receive post-handover datagrams; got {stray:?}"
    );
}
