use core::{
    cell::UnsafeCell,
    fmt, hint,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::addr_of,
    sync::atomic::{AtomicU64, Ordering},
};

use bytemuck::Zeroable;

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
#[repr(C)]
pub struct SpinMutex<T> {
    data: UnsafeCell<T>,
    /// We store hartid+1 in this field, 0 for unlocked. (It's fine to
    /// use hartid+1 as we definitely won't have 18 quintillion and a
    /// bit (2^64-1) cores available.)
    locked: AtomicU64,
}

// SAFETY: See below.
unsafe impl<T: Zeroable> Zeroable for SpinMutex<T> {
    fn zeroed() -> Self {
        // SAFETY: If our inner type is zeroable, then we are
        // zeroable--a locked flag of 0 means unlocked.
        unsafe { core::mem::zeroed() }
    }
}

// SAFETY: `SpinMutex`es only provide mutually exclusive access to their data,
// and their locking state is atomic and thus does not provide simultaneous
// mutable access due to race conditions.
unsafe impl<T: Send> Send for SpinMutex<T> {}
// SAFETY: We only need to have T: Send because we may be able to send
// the T between threads through a `&mut T`, but we can't use the
// Mutex to get a `&T`.
unsafe impl<T: Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicU64::new(0),
            data: UnsafeCell::new(data),
        }
    }

    #[inline]
    pub fn to_components(&self) -> (*mut T, *const AtomicU64) {
        (self.data.get(), addr_of!(self.locked))
    }

    /// Forcibly unlock this `SpinMutex`.
    ///
    /// # Safety
    ///
    /// This function is **very unsafe** as it allows unlocking the
    /// mutex even while it is locked, allowing multiple mutable
    /// references. Use with extreme caution.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(0, Ordering::Release);
    }

    /// Try to lock the `SpinMutex`, returning `None` if it is not
    /// available.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    #[track_caller]
    pub fn try_lock(&'_ self) -> Option<SpinMutexGuard<'_, T>> {
        // Disable interrupts to prevent deadlocks. Interrupts will
        // always be disabled before TLS is enabled, as it's enabled
        // very early (before paging, even).
        let hartid = if hart_local::enabled() {
            LOCAL_HART.with(|hart| {
                hart.push_off();
                hart.hartid.get()
            })
        } else {
            hartid()
        };

        // Try to set the locked flag to our hartid, if it was
        // unlocked.
        //
        // The Acquire success ordering ensures that a Release-store
        // happens-before a successful Acquire-load. We don't need
        // anything special for the failure ordering, as we don't
        // synchronize over anything with that.
        //
        // N.B. We don't use `compare_exchange_weak` which can
        // spuriously fail, which we don't want this function to do.
        // We also don't want to use atomic swap as that would stack
        // mutable borrows if this hart was already holding the lock,
        // which is mega-unsound. Thus we need to ensure it was
        // **definitely** unlocked when we swap, not just lock it
        // anyway.
        if let Err(held_by) =
            self.locked
                .compare_exchange(0, hartid + 1, Ordering::Acquire, Ordering::Relaxed)
        {
            assert!(
                held_by != hartid + 1,
                "SpinMutex::try_lock: deadlock detected"
            );
            // Make sure we re-enable interrupts if we didn't lock.
            if hart_local::enabled() {
                LOCAL_HART.with(hart_local::HartCtx::pop_off);
            }
            return None;
        }

        Some(SpinMutexGuard {
            mutex: self,
            _phantom: PhantomData,
        })
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

        // Try to set the locked flag to our hartid, if it was
        // unlocked. Spin if we can't.
        //
        // The Acquire success ordering ensures that a Release-store
        // happens-before a successful Acquire-load. We don't need
        // anything special for the failure ordering, as we don't
        // synchronize over anything with that.
        //
        // N.B. We don't use `compare_exchange_weak` which can
        // spuriously fail, which may cause misdetection of deadlock.
        // We also don't want to use atomic swap as that would stack
        // mutable borrows if this hart was already holding the lock,
        // which is mega-unsound. Thus we need to ensure it was
        // **definitely** unlocked when we swap, not just lock it
        // anyway.
        while let Err(held_by) =
            self.locked
                .compare_exchange(0, hartid + 1, Ordering::Acquire, Ordering::Relaxed)
        {
            assert!(held_by != hartid + 1, "SpinMutex::lock: deadlock detected");
            hint::spin_loop();
        }

        SpinMutexGuard {
            mutex: self,
            _phantom: PhantomData,
        }
    }
}

impl<T: Default> Default for SpinMutex<T> {
    fn default() -> Self {
        SpinMutex::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for SpinMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(lock) => f.debug_struct("SpinMutex").field("data", &*lock).finish(),
            None => f
                .debug_struct("SpinMutex")
                .field("data", &"<locked>")
                .field(
                    "held_by",
                    &self.locked.load(Ordering::Relaxed).saturating_sub(1),
                )
                .finish_non_exhaustive(),
        }
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

        // Set the locked flag to false. The Release ordering ensures
        // that this unlock happens-before any Acquire-load locks.
        self.mutex.locked.store(0, Ordering::Release);

        // If interrupts were previously enabled, re-enable them.
        // Interrupts will always be disabled before TLS is enabled,
        // as it's enabled very early (before paging, even).
        if hart_local::enabled() {
            LOCAL_HART.with(hart_local::HartCtx::pop_off);
        }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for SpinMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v: &T = self;
        v.fmt(f)
    }
}
