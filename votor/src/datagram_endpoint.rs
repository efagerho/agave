//! Votor-flavored constructor for [`solana_quic_datagram::QuicDatagramEndpoint`].
//!
//! Centralizes the wire-format identifier (`ALPENGLOW_ALPN`) and the metrics
//! namespace used by both directions of alpenglow consensus traffic, so the
//! send side (`voting_service`) and the receive side (the BLS sigverifier
//! consuming `ingress`) cannot drift apart.
//!
//! One UDP socket — one endpoint — multiplexes votes (egress) and inbound
//! consensus messages (ingress) per the lex-pubkey direction rule. See the
//! `solana-quic-datagram` crate docs for the underlying semantics.

use {
    crossbeam_channel::Sender,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_quic_datagram::{
        Admission, Banlist, Error, StakedNodesAdmission,
        endpoint::{Datagram, QuicDatagramEndpoint},
    },
    solana_runtime::bank_forks::BankForks,
    std::{
        collections::HashSet,
        net::UdpSocket,
        sync::{Arc, RwLock},
    },
};

/// ALPN identifier negotiated on every alpenglow QUIC handshake. Changing
/// this value is a wire-breaking protocol change — peers with mismatched
/// ALPN fail the TLS handshake.
pub const ALPENGLOW_ALPN: &[u8] = b"alpenglow-v1";

/// Construct a [`QuicDatagramEndpoint`] tuned for alpenglow consensus
/// traffic. Caller owns admission, banlist, ingress and the banlist
/// eviction receiver; the returned endpoint owns its control loop task.
///
/// **Identity rotation** — register `endpoint.key_updater.clone()` with
/// the validator's `KeyUpdaters` registry (implements
/// `solana_tls_utils::NotifyKeyUpdate`). The intended production
/// pattern is hot-spare failover: a non-voting backup node running
/// with a throwaway keypair is handed the primary's staked keypair to
/// promote it. The new identity is already in every peer's admission
/// set (it's been staked all along) so handshakes pass immediately;
/// the primary observes its connections being replaced by HANDOVER on
/// each peer and exits via the [`crate::handover_shutdown`] threshold.
///
/// **Handover monitoring** — drain `endpoint.handover_events` (each
/// item = a peer pubkey that has accepted a different connection from
/// our identity). [`crate::handover_shutdown::spawn`] aggregates these,
/// stake-weights them against the working bank's epoch, and sets the
/// validator's exit flag once `HANDOVER_SHUTDOWN_STAKE_NUMERATOR/
/// DENOMINATOR` of the cluster has handed us over.
#[allow(clippy::too_many_arguments)]
pub fn spawn<A: Admission>(
    runtime: &tokio::runtime::Handle,
    keypair: &Keypair,
    socket: UdpSocket,
    ingress: Sender<Datagram>,
    admission: Arc<A>,
    banlist: Arc<Banlist<Pubkey>>,
) -> Result<QuicDatagramEndpoint, Error> {
    QuicDatagramEndpoint::new(
        runtime,
        keypair,
        socket,
        ALPENGLOW_ALPN,
        ingress,
        admission,
        banlist,
    )
}

/// Build the set of validator pubkeys whose stake is positive in the
/// working bank's current epoch. This is the canonical admission set;
/// callers seed `StakedNodesAdmission` from this at construction and
/// then drive subsequent epoch-boundary refreshes from
/// [`crate::staked_validators_cache::StakedValidatorsCache`].
pub fn current_admit_set(bank_forks: &Arc<RwLock<BankForks>>) -> HashSet<Pubkey> {
    let bank = bank_forks.read().unwrap().working_bank();
    let epoch = bank.epoch();
    bank.epoch_staked_nodes(epoch)
        .map(|m| {
            m.iter()
                .filter(|(_, stake)| **stake > 0)
                .map(|(pk, _)| *pk)
                .collect()
        })
        .unwrap_or_default()
}

/// Build a fresh [`StakedNodesAdmission`] seeded with the current
/// epoch's staked-set. Hand the returned Arc both to
/// [`spawn`] and to the [`StakedValidatorsCache`] that voting_service
/// owns — the cache will call `.swap()` on epoch transitions.
pub fn build_admission(bank_forks: &Arc<RwLock<BankForks>>) -> Arc<StakedNodesAdmission> {
    Arc::new(StakedNodesAdmission::new(current_admit_set(bank_forks)))
}
