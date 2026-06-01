//! Per-connection reader task. Shared by server-accepted and client-dialed
//! connections - both push received datagrams into the same ingress channel.

use {
    crate::{
        BURST_DATAGRAMS_PER_SECOND_PER_PEER, Banlist, MAX_DATAGRAMS_PER_SECOND_PER_PEER,
        close_codes,
        endpoint::Datagram,
        error::Error,
        stats::{QuicDatagramStats, record_error},
    },
    crossbeam_channel::{Sender, TrySendError},
    log::debug,
    quinn::Connection,
    solana_net_utils::token_bucket::TokenBucket,
    solana_pubkey::Pubkey,
    std::{
        net::SocketAddr,
        sync::{Arc, atomic::Ordering},
    },
};

/// Drive the per-connection read loop to completion. Returns when the
/// connection closes (peer-initiated, banlist-trip, or ingress disconnect).
/// Caller is responsible for reaping the connection table entry afterwards.
pub(crate) async fn read_datagram_loop(
    connection: Connection,
    peer: Pubkey,
    remote_addr: SocketAddr,
    ingress: Sender<Datagram>,
    banlist: Arc<Banlist<Pubkey>>,
    stats: Arc<QuicDatagramStats>,
) {
    // Per-connection rate limiter. Any datagram arriving with the bucket
    // empty is dropped. We do NOT close the connection here, honest peers
    // legitimately burst above the refill rate during catch-up.
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
                record_error(&Error::from(e), &stats);
                break;
            }
        }
    }
}
