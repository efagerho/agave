//! Banning a peer mid-stream evicts every cached connection for that peer and
//! prevents any re-handshake until the ban expires.

#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_quic_datagram::{BAN_DURATION_SHORT, admission::AllowAll, endpoint::Datagram},
    std::{sync::Arc, time::Duration},
};

#[test]
fn ban_evicts_existing_and_blocks_rehandshake() {
    let rt = make_runtime();
    let server = spawn_node(&rt, Arc::new(AllowAll));
    // Lex tiebreaker: dialer's pubkey must be lower than listener's.
    let client = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&server.pubkey()));

    // Establish a connection by driving send-until-receive: the trigger
    // packet is dropped; the retry through the now-Established slot lands.
    let probe = Bytes::from_static(b"probe");
    send_until_received(
        &rt,
        &client.endpoint,
        server.pubkey(),
        server.addr,
        probe.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == probe).then_some(()),
        "first datagram never arrived",
    );
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == probe
    });

    // Ban the client at the server side. Eviction task should close the
    // server-side connection; client's read loop sees ConnectionClosed and
    // exits, dropping its cache entry on the way out.
    server.banlist.ban(client.pubkey(), BAN_DURATION_SHORT);

    // Best-effort wait for eviction to flush. The eviction task is async; on
    // a quiet system it observes the channel within a few ms.
    std::thread::sleep(Duration::from_millis(200));

    // Now have the client send again. The send path dials a new connection
    // (server-side cache no longer holds the old one); but the server is
    // banning this pubkey, so handshake closes with BANNED and no datagram
    // ever reaches ingress.
    let again = Bytes::from_static(b"after-ban");
    rt.block_on(async {
        client
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: server.pubkey(),
                peer_address: server.addr,
                message: again.clone(),
            })
            .await
            .unwrap();
    });

    let bad = server.ingress_rx.recv_timeout(Duration::from_millis(1500));
    assert!(
        bad.is_err(),
        "banned client must not deliver datagrams to server; got {bad:?}"
    );
}
