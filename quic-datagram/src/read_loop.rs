//! Per-connection reader task. Shared by server-accepted and client-dialed
//! connections - both push received datagrams into the same ingress channel.

use {
    crate::{
        BAN_DURATION_SHORT, BURST_DATAGRAMS_PER_SECOND_PER_PEER, Banlist,
        MAX_DATAGRAMS_PER_SECOND_PER_PEER, close_codes,
        endpoint::Datagram,
        error::Error,
        stats::{QuicDatagramStats, record_error},
    },
    crossbeam_channel::{Sender, TrySendError},
    log::{debug, warn},
    quinn::{Connection, ConnectionError},
    solana_net_utils::token_bucket::TokenBucket,
    solana_pubkey::Pubkey,
    std::{
        net::SocketAddr,
        sync::{Arc, atomic::Ordering},
    },
    tokio::sync::mpsc,
};

/// Drive the per-connection read loop to completion. Returns when the
/// connection closes (peer-initiated, banlist-trip, ingress disconnect,
/// HANDOVER). Caller is responsible for reaping the connection table entry
/// afterwards.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_datagram_loop(
    connection: Connection,
    peer: Pubkey,
    remote_addr: SocketAddr,
    ingress: Sender<Datagram>,
    banlist: Arc<Banlist<Pubkey>>,
    handover_events: mpsc::Sender<Pubkey>,
    stats: Arc<QuicDatagramStats>,
) {
    // Per-connection rate limiter. Any datagram arriving with the bucket
    // empty is dropped. We do NOT close the connection or ban the peer here,
    // honest peers legitimately burst above the refill rate during catch-up.
    let rate_limit = TokenBucket::new(
        BURST_DATAGRAMS_PER_SECOND_PER_PEER,
        BURST_DATAGRAMS_PER_SECOND_PER_PEER,
        MAX_DATAGRAMS_PER_SECOND_PER_PEER,
    );
    loop {
        match connection.read_datagram().await {
            Ok(bytes) => {
                // Banlist check happens AFTER the read so a ban that
                // lands while we're awaiting can't let a follow-up
                // datagram leak through to ingress.
                if banlist.is_banned(&peer) {
                    close_codes::BANNED.close(&connection);
                    break;
                }
                if rate_limit.consume_tokens(1).is_err() {
                    drop(bytes);
                    stats.datagram_rate_limited.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                match ingress.try_send(Datagram {
                    peer_pubkey: peer,
                    peer_address: remote_addr,
                    message: bytes,
                }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        stats
                            .datagram_ingress_dropped_channel_full
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        debug!("ingress disconnected; reader for {peer} exiting");
                        break;
                    }
                }
            }
            Err(e) => {
                if matches!(&e, ConnectionError::ApplicationClosed(c) if c.error_code == close_codes::HANDOVER.code)
                {
                    handle_handover(peer, &banlist, &handover_events, &stats);
                }
                record_error(&Error::from(e), &stats);
                break;
            }
        }
    }
}

/// Special handling for `HANDOVER` close: when the peer closes with that
/// application close code, soft-ban the peer in our local banlist (so we
/// don't try to reconnect and disrupt the cluster that has decided to use a
/// different instance of our identity) and forward the peer's pubkey on the
/// caller-supplied `handover_events` channel so the consensus layer can
/// decide whether to shut the node down.
fn handle_handover(
    peer: Pubkey,
    banlist: &Banlist<Pubkey>,
    handover_events: &mpsc::Sender<Pubkey>,
    stats: &QuicDatagramStats,
) {
    stats.handover_received.fetch_add(1, Ordering::Relaxed);
    // Soft-ban so our client side stops dialing this peer - the peer has
    // already informed us that a different instance of our identity is online,
    // and re-dialing would allow us to double vote.
    banlist.ban(peer, BAN_DURATION_SHORT);
    if let Err(mpsc::error::TrySendError::Full(_)) = handover_events.try_send(peer) {
        stats
            .handover_events_channel_full
            .fetch_add(1, Ordering::Relaxed);
        warn!(
            "handover_events channel full; not forwarding handover from {peer} (we were replaced; \
             soft-ban still applied)"
        );
    }
}
