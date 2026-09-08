use {
    crate::crds::Crds,
    parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    std::{
        ops::{Deref, DerefMut},
        sync::{
            LockResult, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
    },
};

/// A task-fair `RwLock<Crds>` with std-compatible poisoning behavior.
pub struct CrdsRwLock {
    inner: RwLock<Crds>,
    poisoned: AtomicBool,
}

impl CrdsRwLock {
    pub fn new(crds: Crds) -> Self {
        Self {
            inner: RwLock::new(crds),
            poisoned: AtomicBool::new(false),
        }
    }

    pub fn read(&self) -> LockResult<CrdsReadGuard<'_>> {
        let guard = CrdsReadGuard {
            guard: self.inner.read(),
        };
        if self.is_poisoned() {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    pub fn write(&self) -> LockResult<CrdsWriteGuard<'_>> {
        let guard = CrdsWriteGuard {
            lock: self,
            guard: self.inner.write(),
            panicking_on_acquire: std::thread::panicking(),
        };
        if self.is_poisoned() {
            Err(PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    pub fn clear_poison(&self) {
        self.poisoned.store(false, Ordering::Release);
    }
}

impl Default for CrdsRwLock {
    fn default() -> Self {
        Self::new(Crds::default())
    }
}

pub struct CrdsReadGuard<'a> {
    guard: RwLockReadGuard<'a, Crds>,
}

impl Deref for CrdsReadGuard<'_> {
    type Target = Crds;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

pub struct CrdsWriteGuard<'a> {
    lock: &'a CrdsRwLock,
    guard: RwLockWriteGuard<'a, Crds>,
    panicking_on_acquire: bool,
}

impl Deref for CrdsWriteGuard<'_> {
    type Target = Crds;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for CrdsWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for CrdsWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.panicking_on_acquire && std::thread::panicking() {
            self.lock.poisoned.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::panic::AssertUnwindSafe};

    #[test]
    fn test_clean_write_during_unwind_does_not_poison() {
        struct Cleanup<'a>(&'a CrdsRwLock);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                drop(self.0.write().unwrap());
            }
        }
        let lock = CrdsRwLock::default();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _cleanup = Cleanup(&lock);
            panic!("unrelated outer panic");
        }));
        assert!(result.is_err());
        assert!(!lock.is_poisoned());
        assert!(lock.read().is_ok());
        assert!(lock.write().is_ok());
    }

    #[test]
    fn test_read_panic_does_not_poison() {
        let lock = CrdsRwLock::default();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = lock.read().unwrap();
            panic!("read panic");
        }));
        assert!(result.is_err());
        assert!(!lock.is_poisoned());
        assert!(lock.write().is_ok());
    }

    #[test]
    fn test_write_panic_poisons_lock() {
        let lock = CrdsRwLock::default();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = lock.write().unwrap();
            panic!("poison the lock");
        }));
        assert!(result.is_err());
        assert!(lock.is_poisoned());
        assert!(lock.read().is_err());
        assert!(lock.write().is_err());

        lock.clear_poison();
        assert!(!lock.is_poisoned());
        assert!(lock.read().is_ok());
    }
}
