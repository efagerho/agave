//! Identity rotation: replacing the keypair backing an endpoint must
//!   1. evict all cached connections (so peers re-handshake against the
//!      new cert),
//!   2. start using the new pubkey for both TLS attestation and the
//!      lex-direction rule,
//!   3. let subsequent egress flow under the new identity.
//!
//! Setup: two endpoints A (client) and B (server) with AllowAll
//! admission so neither rejects on stake membership. A's initial keypair
//! K1 and rotated keypair K2 are both deterministically generated below
//! B's pubkey so A is always the lex-correct dialer on both sides of
//! the rotation.

#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_keypair::Signer,
    solana_quic_datagram::admission::AllowAll,
    solana_tls_utils::NotifyKeyUpdate,
    std::{sync::Arc, time::Duration},
};

#[test]
fn rotation_evicts_connections_and_resends_under_new_identity() {
    let rt = make_runtime();
    let server = spawn_node(&rt, Arc::new(AllowAll));

    let k1 = keypair_below(&server.pubkey());
    let k1_pk = k1.pubkey();
    let client = spawn_node_with(&rt, Arc::new(AllowAll), k1);

    // Send under K1. Server should observe message attributed to K1.
    let p1 = Bytes::from_static(b"under-K1");
    send_until_received(
        &rt,
        &client.endpoint,
        server.pubkey(),
        server.addr,
        p1.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.peer_pubkey == k1_pk && d.message == p1).then_some(()),
        "server never received message attributed to K1",
    );
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == p1
    });

    // Rotate to K2. Pick K2 also below the server so dial direction stays.
    let k2 = keypair_below(&server.pubkey());
    let k2_pk = k2.pubkey();
    assert_ne!(k1_pk, k2_pk, "K1 and K2 must differ");
    client
        .endpoint
        .key_updater
        .update_key(&k2)
        .expect("identity rotation accepted");

    // The control loop applies the rotation asynchronously: rebuild TLS
    // configs, evict cached connections (server sees IDENTITY_ROTATED).
    // Give it a beat.
    std::thread::sleep(Duration::from_millis(500));

    // Send under K2. Server's table no longer holds the K1 entry; a
    // fresh handshake under K2 establishes a new connection, and the
    // server observes the message attributed to K2.
    let p2 = Bytes::from_static(b"under-K2");
    send_until_received(
        &rt,
        &client.endpoint,
        server.pubkey(),
        server.addr,
        p2.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.peer_pubkey == k2_pk && d.message == p2).then_some(()),
        "server never received message attributed to K2 after rotation",
    );
}
