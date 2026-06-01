//! End-to-end admission: server admits one pubkey, rejects the other.

#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{drain_matching, keypair_below, make_runtime, send_until_received, spawn_node_with},
    solana_keypair::{Keypair, Signer},
    solana_quic_datagram::{
        admission::{AllowAll, StakedNodesAdmission},
        endpoint::Datagram,
    },
    std::{collections::HashSet, sync::Arc, time::Duration},
};

#[test]
fn staked_peer_is_admitted_unstaked_is_rejected() {
    let rt = make_runtime();

    // Lex tiebreaker requires the dialing client's pubkey < server's. Pick
    // the server keypair first, then derive client A's keypair below it.
    let server_kp = Keypair::new();
    let server_pk = server_kp.pubkey();
    let a_kp = keypair_below(&server_pk);
    let a_pk = a_kp.pubkey();
    let admit_set: HashSet<_> = std::iter::once(a_pk).collect();
    let server = spawn_node_with(
        &rt,
        Arc::new(StakedNodesAdmission::new(admit_set)),
        server_kp,
    );

    // Client A - admitted (pubkey < server's, so handshake passes lex check).
    let client_a = spawn_node_with(&rt, Arc::new(AllowAll), a_kp);
    // Client B - not admitted. Pick below server so the rejection is by the
    // admission check, not the lex check.
    let client_b = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&server_pk));

    let payload_a = Bytes::from_static(b"hello-from-A");
    send_until_received(
        &rt,
        &client_a.endpoint,
        server.pubkey(),
        server.addr,
        payload_a.clone(),
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.peer_pubkey == a_pk && d.message == payload_a).then_some(()),
        "server never received payload from admitted peer A",
    );
    // send_until_received may have left retry duplicates of payload_a
    // in the channel; drain them before asserting B's rejection.
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == payload_a
    });

    let payload_b = Bytes::from_static(b"hello-from-B");
    rt.block_on(async {
        client_b
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: server.pubkey(),
                peer_address: server.addr,
                message: payload_b.clone(),
            })
            .await
            .expect("egress send B");
    });

    // Server must close the handshake before any datagram from B is queued.
    let bad = server.ingress_rx.recv_timeout(Duration::from_millis(800));
    assert!(
        bad.is_err(),
        "unstaked peer B's datagram should not reach server ingress, got {bad:?}"
    );
}
