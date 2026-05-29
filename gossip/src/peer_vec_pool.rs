//! Thread-local pool of reusable `Vec<ContactInfo>` buffers.
//!
//! `ClusterInfo::repair_peers` (and similar methods) build a peer list per
//! call, returning a fresh `Vec<ContactInfo>` to the caller. The Vec is
//! ~1.86 MiB on mainnet (~3 k entries × ~620 B). That's well above jemalloc's
//! "large" threshold (~14 KiB), so the allocation comes from a fresh arena
//! extent; on free the pages are returned to the OS via
//! `madvise(MADV_DONTNEED)` and the next call faults them in again on
//! first-touch `memcpy`. On a live mainnet-beta validator this accounted for
//! roughly 8 % of all process page faults from the repair service alone.
//!
//! Pre-sizing with `Vec::with_capacity(num_nodes())` removed the
//! realloc-chain portion of that cost (about half), but the first-touch on
//! the final allocation persists as long as the underlying allocation is
//! re-acquired per call. This module replaces the per-call allocation with a
//! per-thread free-list so the backing memory stays resident: the first call
//! on a thread allocates, and every subsequent call reuses the same pages.

use {
    crate::contact_info::ContactInfo,
    std::{
        cell::RefCell,
        mem::ManuallyDrop,
        ops::{Deref, DerefMut},
    },
};

/// Maximum buffers retained per thread. `ClusterInfo::repair_peers` is a leaf
/// call (no nested re-entry to other pooled methods), so in practice the
/// per-thread footprint converges to one buffer. The small cap > 1 leaves
/// headroom for a hypothetical second concurrent checkout on the same
/// thread (e.g. a future caller that holds two peer lists alive at once),
/// preventing it from forcing a fresh allocation on every call.
const POOL_CAP: usize = 4;

thread_local! {
    static POOL: RefCell<Vec<Vec<ContactInfo>>> = const { RefCell::new(Vec::new()) };
}

/// A `Vec<ContactInfo>` checked out of the per-thread pool. Derefs to
/// `Vec<ContactInfo>` (and through that to `&[ContactInfo]`), so callers
/// that use the result via slice borrow or `iter()` work unchanged.
///
/// On drop the buffer is cleared (length set to 0) and returned to the pool
/// with capacity intact, so the next checkout on the same thread reuses the
/// same allocation and the same already-faulted-in pages.
pub struct PooledPeerVec {
    inner: ManuallyDrop<Vec<ContactInfo>>,
}

impl PooledPeerVec {
    /// Check out a buffer with at least `min_capacity` slots. Pops a buffer
    /// from the per-thread pool if one is available with sufficient
    /// capacity; otherwise allocates a fresh one. The returned buffer is
    /// empty (`len() == 0`) regardless of source.
    pub fn checkout(min_capacity: usize) -> Self {
        let inner = POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            // Pop the most recently returned buffer (LIFO — warmest cache).
            if let Some(mut v) = pool.pop() {
                v.clear();
                if v.capacity() < min_capacity {
                    v.reserve(min_capacity - v.len());
                }
                v
            } else {
                Vec::with_capacity(min_capacity)
            }
        });
        Self {
            inner: ManuallyDrop::new(inner),
        }
    }
}

impl Deref for PooledPeerVec {
    type Target = Vec<ContactInfo>;
    fn deref(&self) -> &Vec<ContactInfo> {
        &self.inner
    }
}

impl DerefMut for PooledPeerVec {
    fn deref_mut(&mut self) -> &mut Vec<ContactInfo> {
        &mut self.inner
    }
}

impl Drop for PooledPeerVec {
    fn drop(&mut self) {
        // SAFETY: `self.inner` is not accessed after this point; we move it
        // out and either return it to the pool or let it drop normally.
        let v = unsafe { ManuallyDrop::take(&mut self.inner) };
        POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < POOL_CAP {
                pool.push(v);
            }
            // Pool is full — let `v` drop and free its allocation. This caps
            // per-thread memory regardless of how many transient buffers
            // get materialised in pathological re-entry scenarios.
        });
    }
}

impl IntoIterator for PooledPeerVec {
    type Item = ContactInfo;
    type IntoIter = std::vec::IntoIter<ContactInfo>;

    /// Consume the buffer by value. The backing allocation is consumed by
    /// the returned iterator (and freed when it drops); the pool does *not*
    /// get this buffer back. Callers that want to keep pooling should use
    /// `.iter()` (via `Deref`) instead.
    fn into_iter(mut self) -> std::vec::IntoIter<ContactInfo> {
        // SAFETY: `self.inner` is not used after this; `mem::forget`
        // suppresses the `Drop` impl so the moved-out `Vec` isn't aliased.
        let v = unsafe { ManuallyDrop::take(&mut self.inner) };
        std::mem::forget(self);
        v.into_iter()
    }
}

impl<'a> IntoIterator for &'a PooledPeerVec {
    type Item = &'a ContactInfo;
    type IntoIter = std::slice::Iter<'a, ContactInfo>;
    fn into_iter(self) -> std::slice::Iter<'a, ContactInfo> {
        self.inner.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_returns_empty_buffer() {
        let buf = PooledPeerVec::checkout(64);
        assert!(buf.is_empty());
        assert!(buf.capacity() >= 64);
    }

    #[test]
    fn pool_reuses_allocation_across_checkouts() {
        let ptr_first;
        let cap_first;
        {
            let buf = PooledPeerVec::checkout(128);
            ptr_first = buf.as_ptr();
            cap_first = buf.capacity();
        }
        // On a fresh thread the only buffer in the pool is the one we just
        // returned, so the next checkout (at most as large) must reuse it.
        let buf = PooledPeerVec::checkout(128);
        assert_eq!(buf.as_ptr(), ptr_first);
        assert_eq!(buf.capacity(), cap_first);
    }

    #[test]
    fn checkout_grows_undersized_pooled_buffer() {
        {
            let _small = PooledPeerVec::checkout(16);
        }
        let big = PooledPeerVec::checkout(1024);
        assert!(big.capacity() >= 1024);
    }

    #[test]
    fn pool_is_bounded() {
        let mut bufs = Vec::new();
        for _ in 0..POOL_CAP * 2 {
            bufs.push(PooledPeerVec::checkout(0));
        }
        // Drop them all — only POOL_CAP should be retained.
        drop(bufs);
        let retained = POOL.with(|p| p.borrow().len());
        assert_eq!(retained, POOL_CAP);
    }
}
