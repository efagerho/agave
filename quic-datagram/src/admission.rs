//! Admission policy: decides whether the local endpoint will accept
//! a TLS-attested peer pubkey.

use {
    arc_swap::ArcSwap,
    solana_pubkey::Pubkey,
    std::{collections::HashSet, sync::Arc},
};

/// Called once per inbound handshake (after the peer cert is parsed) and once
/// per outbound dial-success. A `false` answer closes the connection with the
/// `NOT_ADMITTED` error code.
///
/// Implementations must be cheap - this is on the hot path of every new
/// connection.
pub trait Admission: Send + Sync + 'static {
    fn allow(&self, peer: &Pubkey) -> bool;
}

/// Snapshot of the currently-admitted peer set. Producers (typically
/// the staked-validators cache) call [`StakedNodesAdmission::swap`] to publish
/// a new generation; consumers see it on the next `allow` call.
#[derive(Default)]
pub struct StakedNodesAdmission {
    inner: ArcSwap<HashSet<Pubkey>>,
}

impl StakedNodesAdmission {
    pub fn new(initial: HashSet<Pubkey>) -> Self {
        Self {
            inner: ArcSwap::new(Arc::new(initial)),
        }
    }

    /// Publish a new admit-set. Atomic; no readers block.
    pub fn swap(&self, next: HashSet<Pubkey>) {
        self.inner.store(Arc::new(next));
    }

    /// Number of admitted peers in the current generation.
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.load().is_empty()
    }
}

impl Admission for StakedNodesAdmission {
    fn allow(&self, peer: &Pubkey) -> bool {
        self.inner.load().contains(peer)
    }
}

/// Admit every peer. **Test/bench use only** - gated behind
/// `dev-context-only-utils` so production binaries cannot accidentally use it.
#[cfg(any(test, feature = "dev-context-only-utils"))]
pub struct AllowAll;

#[cfg(any(test, feature = "dev-context-only-utils"))]
impl Admission for AllowAll {
    fn allow(&self, _: &Pubkey) -> bool {
        true
    }
}
