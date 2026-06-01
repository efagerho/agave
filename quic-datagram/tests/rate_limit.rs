#[path = "common.rs"]
mod common;

use {
    bytes::Bytes,
    common::{
        drain_matching, keypair_below, make_runtime, recv_until, send_until_received, spawn_node,
        spawn_node_with,
    },
    solana_quic_datagram::{
        BURST_DATAGRAMS_PER_SECOND_PER_PEER, admission::AllowAll, endpoint::Datagram,
    },
    std::{sync::Arc, time::Duration},
};

#[test]
/// Per-connection receive token bucket. A burst beyond
/// `BURST_DATAGRAMS_PER_SECOND_PER_PEER` is *dropped* silently - the
/// bucket itself is the throttle. The connection stays alive and the
/// peer is NOT banned (consensus traffic legitimately bursts above the
/// refill rate during catch-up).
fn burst_exceeding_rate_limit_drops_excess_without_banning() {
    let rt = make_runtime();
    let server = spawn_node(&rt, Arc::new(AllowAll));
    let client = spawn_node_with(&rt, Arc::new(AllowAll), keypair_below(&server.pubkey()));

    // Establish the connection: retry the probe until one lands (first one
    // triggers the dial, gets dropped; followers ride the Established slot).
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
    // Drain probe retry duplicates so they don't inflate the post-burst
    // delivery count below.
    drain_matching(&server.ingress_rx, Duration::from_millis(200), |d| {
        d.message == probe
    });

    // Blast a burst far in excess of the bucket capacity. Probe retries
    // have already consumed some tokens; the (capacity)-th additional
    // datagram trips the limiter.
    let burst = (BURST_DATAGRAMS_PER_SECOND_PER_PEER as usize) * 4;
    rt.block_on(async {
        for i in 0..burst {
            let payload = Bytes::from(format!("burst-{i:04}").into_bytes());
            client
                .endpoint
                .egress
                .send(Datagram {
                    peer_pubkey: server.pubkey(),
                    peer_address: server.addr,
                    message: payload,
                })
                .await
                .unwrap();
        }
    });

    // Let the receiver chew through whatever fits in the bucket.
    std::thread::sleep(Duration::from_millis(500));

    // The peer must NOT have been banned - drop-only semantics.
    assert!(
        !server.banlist.is_banned(&client.pubkey()),
        "client pubkey should NOT be banned by RX rate limit (drop-only semantics)"
    );

    // Drain whatever made it through ingress. The bucket caps how many
    // post-probe datagrams can be delivered within the first refill window.
    let mut delivered = 0usize;
    while server
        .ingress_rx
        .recv_timeout(Duration::from_millis(50))
        .is_ok()
    {
        delivered = delivered.saturating_add(1);
    }
    let cap = BURST_DATAGRAMS_PER_SECOND_PER_PEER as usize;
    assert!(
        delivered <= cap + 2,
        "delivered {delivered} datagrams post-probe, exceeds bucket capacity {cap} + slop"
    );

    // After waiting long enough for the bucket to refill, the sender should
    // be able to deliver fresh datagrams on the SAME connection (proves the
    // connection wasn't torn down).
    std::thread::sleep(Duration::from_secs(2));
    let resume = Bytes::from_static(b"after-refill");
    rt.block_on(async {
        client
            .endpoint
            .egress
            .send(Datagram {
                peer_pubkey: server.pubkey(),
                peer_address: server.addr,
                message: resume.clone(),
            })
            .await
            .unwrap();
    });
    recv_until(
        &server.ingress_rx,
        Duration::from_secs(5),
        |d| (d.message == resume).then_some(()),
        "post-refill datagram never arrived - connection may have been torn down",
    );
}
