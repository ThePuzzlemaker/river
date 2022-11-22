use core::{
    cell::{Cell, UnsafeCell},
    hint,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use alloc::fmt;

use crate::{
    asm::{self, hartid},
    hart_local::{self, LOCAL_HART},
};

/// A Mutex backed by a spin-lock.
///
/// # Caution
///
/// It is important to **never drop locks out-of-order**:
/// ```rust,ignore
/// let guard1 = lock1.lock();
/// let guard2 = lock2.lock();
/// // ... //
/// drop(guard1);
/// drop(guard2);
/// ```
///
/// Due to how the `SpinLock` is implemented, this may cause deadlocks by
/// accidentally allowing interrupts to occur. For this reason, **always**
/// block-scope your locks so that they are dropped in the correct order.
///
/// Additionally, do not enable interrupts while holding a lock. This may cause
/// deadlocks due to the same reason.
///
/// These behaviours are not unsafe or unsound, but they may cause deadlocks,
/// which are undesirable.
// TODO: SpinRwLock?
pub struct SpinMutex<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
    held_by: Cell<Option<u64>>,
}

// SAFETY: `SpinMutex`es only provide mutually exclusive access to their data,
// and their locking state is atomic and thus does not provide simultaneous
// mutable access due to race conditions. (TODO: properly verify atomics--I
// *think* they're right...(?) (famous last words))
unsafe impl<T: Sync + Send> Send for SpinMutex<T> {}
// SAFETY: See above.
unsafe impl<T: Sync + Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
            held_by: Cell::new(None),
        }
    }

    /// Lock the `SpinMutex`, potentially spinning if it is not available.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    #[track_caller]
    pub fn lock(&'_ self) -> SpinMutexGuard<'_, T> {
        // Disable interrupts to prevent deadlocks.
        // Interrupts will always be disabled before TLS is enabled,
        // as it's enabled very early (before paging, even).
        let hartid = if hart_local::enabled() {
            LOCAL_HART.with(|hart| {
                hart.push_off();
                hart.hartid.get()
            })
        } else {
            hartid()
        };
        assert!(
            self.held_by.get() != Some(hartid),
            "SpinMutex::lock: deadlock detected"
        );

        // Try to set the locked flag to `true` if it was `false`.
        // We loop if it could not be done, or if the previous value was
        // somehow `true`.
        // The Acquire success ordering ensures that any other loops will
        // see this acquisition before they load the locked flag.
        // The Relaxed failure ordering means that it's okay to reorder loads
        // and stores across failed iterations--as no synchronization is
        // necessary there as we do not have access to the data.
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }

        self.held_by.set(Some(hartid));

        SpinMutexGuard {
            mutex: self,
            _phantom: PhantomData,
        }
    }
}

impl<T> fmt::Debug for SpinMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: make this better
        write!(f, "SpinMutex(<opaque>)")
    }
}

pub struct SpinMutexGuard<'a, T> {
    mutex: &'a SpinMutex<T>,
    _phantom: PhantomData<&'a mut T>,
}

impl<'a, T> Deref for SpinMutexGuard<'a, T> {
    type Target = T;

    fn deref(&'_ self) -> &'_ Self::Target {
        // SAFETY: The data is valid due to our invariants.
        // Additionally, we can return a &'_ T as we have a &'_ Self.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T> DerefMut for SpinMutexGuard<'a, T> {
    fn deref_mut(&'_ mut self) -> &'_ mut Self::Target {
        // SAFETY: The data is valid due to our invariants.
        // Additionally, we can return a &'_ mut T as we have a &'_ mut Self.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T> Drop for SpinMutexGuard<'a, T> {
    fn drop(&mut self) {
        // Make sure, just in case, that interrupts are still off.
        asm::intr_off();

        // The held_by field is non-normative with respect to lock status, so
        // it's fine to mutate it before we've released the lock.
        self.mutex.held_by.set(None);

        // Set the locked flag to false.
        // The Release ordering ensures that the acquisition of this guard
        // cannot occur after its release.
        self.mutex.locked.store(false, Ordering::Release);

        // If interrupts were previously enabled, re-enable them.
        // Interrupts will always be disabled before TLS is enabled,
        // as it's enabled very early (before paging, even).
        if hart_local::enabled() {
            LOCAL_HART.with(hart_local::HartCtx::pop_off);
        }
    }
}
