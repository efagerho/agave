//! Gossip bombardment tool: send Protocol::PushMessage packets containing
//! every CrdsData variant (including deprecated ones) to a target endpoint.
//!
//! ## Why ping/pong matters
//!
//! `verify_gossip_addr` (cluster_info.rs) applies to every incoming push message:
//! - Any CrdsData variant other than ContactInfo is passed through unchanged.
//! - ContactInfo is accepted only when (a) its gossip address is a valid,
//!   non-unspecified address and (b) that (pubkey, addr) pair has been verified
//!   through a Ping→Pong exchange with the sending node.
//!
//! Separately, `discard_different_shred_version` drops non-ContactInfo values
//! from pubkeys that have no CRDS record, which means ContactInfo must be
//! accepted first before any other variant can reach the CRDS table.
//!
//! Therefore the bombard function:
//! 1. Resolves its actual outgoing IP so ContactInfo carries a routable address.
//! 2. Runs a background thread that replies to incoming Ping messages with Pong.
//! 3. Repeatedly sends ContactInfo until the ping/pong exchange completes and
//!    the entry is accepted into the target's CRDS.
//! 4. Then floods all CrdsData variants in the main loop.
use {
    crate::{
        contact_info::ContactInfo,
        crds_data::CrdsData,
        crds_value::CrdsValue,
        ping_pong::Pong,
        protocol::Protocol,
    },
    bincode,
    rand,
    solana_keypair::Keypair,
    solana_packet::PACKET_DATA_SIZE,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_time_utils::timestamp,
    std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    },
};

fn send_push(socket: &UdpSocket, endpoint: SocketAddr, keypair: &Keypair, data: CrdsData, round: u64) {
    let pubkey: Pubkey = keypair.pubkey();
    let value = CrdsValue::new(data, keypair);
    let label = format!("{:?}", value.label());
    let message = Protocol::PushMessage(pubkey, vec![value]);
    match bincode::serialize(&message) {
        Ok(bytes) if bytes.len() <= PACKET_DATA_SIZE => match socket.send_to(&bytes, endpoint) {
            Ok(_) => println!("round {round}: sent {label} ({} bytes)", bytes.len()),
            Err(e) => eprintln!("round {round}: send error for {label}: {e}"),
        },
        Ok(bytes) => eprintln!(
            "round {round}: {label} too large ({} > {PACKET_DATA_SIZE})",
            bytes.len()
        ),
        Err(e) => eprintln!("round {round}: serialize error for {label}: {e}"),
    }
}

/// Receive loop that replies to every incoming `PingMessage` with a `PongMessage`.
///
/// This is required so the target node's ping cache accepts our gossip address,
/// which in turn allows our `ContactInfo` to pass `verify_gossip_addr` and be
/// inserted into the target's CRDS table.
fn run_ping_responder(socket: UdpSocket, keypair: Arc<Keypair>, exit: Arc<AtomicBool>) {
    let mut buf = [0u8; PACKET_DATA_SIZE];
    while !exit.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                if let Ok(Protocol::PingMessage(ping)) = bincode::deserialize(&buf[..len]) {
                    let pong = Pong::new(&ping, &keypair);
                    if let Ok(bytes) = bincode::serialize(&Protocol::PongMessage(pong)) {
                        socket.send_to(&bytes, from).ok();
                        println!("ping-pong: replied to ping from {from}");
                    }
                }
            }
            // Timeout / would-block: just loop back and check the exit flag.
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

/// Send [`Protocol::PushMessage`] packets to `endpoint`, cycling through every
/// [`CrdsData`] variant (including deprecated ones) each round.
///
/// * `keypair`       – signing identity; a fresh random keypair is generated
///                     when `None` is supplied.
/// * `shred_version` – must match the target cluster's shred version. Use
///                     `solana_net_utils::get_cluster_shred_version` to
///                     obtain this value automatically.
/// * `count`         – number of bombardment rounds; `None` runs indefinitely.
/// * `rate`          – maximum packets per second across all variants;
///                     `0` means unlimited (send as fast as possible).
pub fn bombard_endpoint(
    endpoint: SocketAddr,
    keypair: Option<Keypair>,
    shred_version: u16,
    count: Option<u64>,
    rate: u64,
) -> std::io::Result<()> {
    let keypair = Arc::new(keypair.unwrap_or_else(Keypair::new));

    let unspecified = if endpoint.is_ipv4() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    };

    // Bind the main socket on a random port.
    let socket = UdpSocket::bind(SocketAddr::new(unspecified, 0))?;
    let local_port = socket.local_addr()?.port();

    // Resolve our actual outgoing IP by temporarily "connecting" a throwaway
    // socket. UDP connect is connectionless — it just asks the OS which source
    // address it would use to reach the destination, so no packets are sent.
    // sanitize_socket rejects 0.0.0.0, so we need the real IP here.
    let local_ip = {
        let tmp = UdpSocket::bind(SocketAddr::new(unspecified, 0))?;
        tmp.connect(endpoint)?;
        tmp.local_addr()?.ip()
    };
    let gossip_addr = SocketAddr::new(local_ip, local_port);

    let packet_interval = (rate > 0).then(|| Duration::from_nanos(1_000_000_000 / rate));

    println!(
        "Bombarding {endpoint} as pubkey {} \
         (shred_version={shred_version}, gossip_addr={gossip_addr}, rate={})",
        keypair.pubkey(),
        if rate == 0 {
            "unlimited".to_string()
        } else {
            format!("{rate} pkt/s")
        },
    );

    // ── Ping responder thread ────────────────────────────────────────────
    // Target nodes verify our gossip address via Ping→Pong before accepting
    // ContactInfo. The responder thread handles that handshake concurrently.
    let exit = Arc::new(AtomicBool::new(false));
    let socket_rx = socket.try_clone()?;
    socket_rx.set_read_timeout(Some(Duration::from_millis(50)))?;
    let responder = {
        let exit = exit.clone();
        let keypair = keypair.clone();
        thread::spawn(move || run_ping_responder(socket_rx, keypair, exit))
    };

    // ── Pre-flight: ContactInfo handshake ────────────────────────────────
    // Build a ContactInfo that carries our real gossip address and the correct
    // shred version. We send it several times so the target has a chance to
    // ping us, receive our pong, and then accept the next ContactInfo.
    let make_contact_info = || {
        let mut ci = ContactInfo::new(keypair.pubkey(), timestamp(), shred_version);
        ci.set_gossip(gossip_addr).ok();
        CrdsData::from(ci)
    };

    // First wave: trigger the target's ping.
    for _ in 0..3 {
        send_push(&socket, endpoint, &keypair, make_contact_info(), 0);
        thread::sleep(Duration::from_millis(50));
    }
    // Allow enough time for the Ping→Pong exchange to complete.
    thread::sleep(Duration::from_millis(300));
    // Second wave: ContactInfo should now pass the ping cache and enter CRDS.
    for _ in 0..3 {
        send_push(&socket, endpoint, &keypair, make_contact_info(), 0);
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(50));

    // ── Main bombardment loop ─────────────────────────────────────────────
    let mut rng = rand::rng();
    let mut next_send = Instant::now();
    let mut round = 0u64;
    loop {
        if let Some(max) = count {
            if round >= max {
                break;
            }
        }

        for data in CrdsData::make_all_crds_data(&mut rng, keypair.pubkey(), shred_version) {
            if let Some(interval) = packet_interval {
                let now = Instant::now();
                if now < next_send {
                    thread::sleep(next_send - now);
                }
                next_send += interval;
            }
            send_push(&socket, endpoint, &keypair, data, round);
        }

        round += 1;
    }

    exit.store(true, Ordering::Relaxed);
    responder.join().ok();
    println!("Bombardment complete after {round} round(s)");
    Ok(())
}
