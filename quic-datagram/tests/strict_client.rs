#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, recv_until, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_quic_datagram::{admission::AllowAll, endpoint::Datagram},
    std::{sync::Arc, time::Duration},
};

#[test]
/// When `local_pubkey >= peer` and no cached
/// connection exists, the egress datagram is dropped (counted as
/// `egress_dropped_higher_pubkey`). Once the lower-pubkey peer dials in,
/// the higher side's egress flows via send_over_inbound on the cached inbound.
fn higher_pubkey_drops_egress_until_lower_dials_in() {
    let rt = make_runtime();
    // B has the higher pubkey, A the lower.
    let b = spawn_node(&rt, Arc::new(AllowAll));
    let a = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&b.pubkey()));

    // B (higher) tries to send to A (lower) before A has dialed B. Per the
    // lex rule, B's client drops the datagram silently - A must not receive.
    let dropped = Bytes::from_static(b"should-be-dropped");
    rt.block_on(async {
        b.endpoint
            .egress
            .send(Datagram {
                peer_pubkey: a.pubkey(),
                peer_address: a.addr,
                message: dropped.clone(),
            })
            .await
            .unwrap();
    });
    let stray = a.ingress_rx.recv_timeout(Duration::from_millis(800));
    assert!(
        stray.is_err(),
        "lower-pubkey peer must not receive a higher-pubkey peer's egress without first dialing \
         in; got {stray:?}"
    );

    // A dials B (correct direction per the lex rule). B's server accepts
    // and caches the inbound from A. Trigger-drop means we need to retry
    // until the dial completes and a follower lands.
    let from_a = Bytes::from_static(b"from-A");
    send_until_received(
        &rt,
        &a.endpoint,
        b.pubkey(),
        b.addr,
        from_a.clone(),
        &b.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == from_a).then_some(()),
        "B did not receive A's dial",
    );
    drain_matching(&b.ingress_rx, Duration::from_millis(200), |d| {
        d.message == from_a
    });

    // B can now reach A via send_over_inbound on the cached inbound. No
    // dial needed — single send is enough.
    let from_b = Bytes::from_static(b"from-B-after-A-dialed");
    rt.block_on(async {
        b.endpoint
            .egress
            .send(Datagram {
                peer_pubkey: a.pubkey(),
                peer_address: a.addr,
                message: from_b.clone(),
            })
            .await
            .unwrap();
    });
    recv_until(
        &a.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == from_b).then_some(()),
        "A did not receive B's datagram after the inbound established a connection",
    );
}
