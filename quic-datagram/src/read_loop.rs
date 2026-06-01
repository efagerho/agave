//! Per-connection reader task. Shared by server-accepted and client-dialed
//! connections - both push received datagrams into the same ingress channel.

use {
    crate::{
        endpoint::Datagram,
        error::Error,
        stats::{QuicDatagramStats, record_error},
    },
    crossbeam_channel::{Sender, TrySendError},
    log::debug,
    quinn::Connection,
    solana_pubkey::Pubkey,
    std::{
        net::SocketAddr,
        sync::{Arc, atomic::Ordering},
    },
};

/// Drive the per-connection read loop to completion. Returns when the
/// connection closes (peer-initiated or ingress disconnect). Caller is
/// responsible for reaping the connection table entry afterwards.
pub(crate) async fn read_datagram_loop(
    connection: Connection,
    peer: Pubkey,
    remote_addr: SocketAddr,
    ingress: Sender<Datagram>,
    stats: Arc<QuicDatagramStats>,
) {
    loop {
        match connection.read_datagram().await {
            Ok(bytes) => match ingress.try_send(Datagram {
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
            },
            Err(e) => {
                record_error(&Error::from(e), &stats);
                break;
            }
        }
    }
}
