use core::{
    cell::UnsafeCell,
    fmt, hint,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    asm::{self, hartid},
    hart_local::{self, LOCAL_HART},
};

/// A `RwLock` backed by a spin-lock.
///
/// # Caution
///
/// It is important to **never drop locks out-of-order**:
/// ```rust,ignore
/// let guard1 = lock1.read();
/// let guard2 = lock2.read();
/// // ... //
/// drop(guard1);
/// drop(guard2);
/// ```
///
/// Due to how the `SpinRwLock` is implemented, this may cause
/// deadlocks by accidentally allowing interrupts to occur. For this
/// reason, **always** block-scope your locks so that they are dropped
/// in the correct order.
///
/// Additionally, do not enable interrupts while holding a lock. This
/// may cause deadlocks due to the same reason.
///
/// These behaviours are not unsafe or unsound, but they may cause
/// deadlocks, which are undesirable.
// TODO: prevent writer starvation?
#[repr(C)]
pub struct SpinRwLock<T> {
    data: UnsafeCell<T>,
    /// The number of readers. If above [`u32::MAX`], then `state -
    /// u32::MAX` is the hart id of the current mutable lock holder.
    state: AtomicU64,
}

// SAFETY: `SpinRwLock`s provide shared access if there is no
// exclusive access, and vice versa. Additionally, the flags to
// determine these statuses are atomic and thus allow thread-safety.
//
// We cannot send a SpinRwLock<T> to another thread unless there are
// no outstanding references, and these references are tracked
// atomically. Thus it can be Send. See below comment for information
// on bounds.
unsafe impl<T: Send + Sync> Send for SpinRwLock<T> {}
// SAFETY: We need to have T: Send + Sync because we may be able to
// send the T between threads through a `&mut T`, and we can use the
// Mutex to get a `&T`.
unsafe impl<T: Send + Sync> Sync for SpinRwLock<T> {}

impl<T> SpinRwLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicU64::new(0),
            data: UnsafeCell::new(data),
        }
    }

    /// Get the data pointer of this `SpinRwLock`. Note that it is
    /// unsafe to read this pointer if a read or write lock is not
    /// held.
    pub const fn data_ptr(&self) -> *const T {
        self.data.get().cast_const()
    }

    /// Forcibly unlock this `SpinRwLock`.
    ///
    /// # Safety
    ///
    /// This function is **very unsafe** as it allows unlocking the
    /// rwlock even while it is locked, allowing multiple mutable
    /// references. Use with extreme caution.
    pub unsafe fn force_unlock(&self) {
        self.state.store(0, Ordering::Release);
    }

    /// Try to lock the `SpinRwLock` as readable, returning `None` if
    /// it is not available.
    ///
    /// # Panics
    ///
    /// This function will panic if the number of readers is
    /// >=`usize::MAX-1`, or if it detects a deadlock.
    #[track_caller]
    pub fn try_read(&'_ self) -> Option<SpinRwLockReadGuard<'_, T>> {
        // Disable interrupts to prevent deadlocks. Interrupts will
        // always be disabled before TLS is enabled, as it's enabled
        // very early (before paging, even)
        let hartid = if hart_local::enabled() {
            LOCAL_HART.with(|hart| {
                hart.push_off();
                hart.hartid.get()
            })
        } else {
            hartid()
        };

        let error: u64;
        let old: u64;

        // Try to increase the number of readers by one, using an
        // Acquire load-reserved and a Relaxed store-conditional. We
        // go into a short loop until the store-conditional succeeds,
        // but first short-circuit if we're for sure locked. This is
        // essentially a cmpxchg loop, but more easy to reason about
        // and likely much more efficient.
        //
        // SAFETY: This code does not break any of our invariants, and
        // does not cause undefined behaviour.
        unsafe {
            core::arch::asm!(
		"
0:
# try to acquire-load state
lr.d.aq t0, ({state})

# if state >= u32::MAX, fail
# (locked mutable)
bge t0, {halfway}, 1f

# try to store-relaxed t0+1
# if s has since changed, this will fail.
# we go into a short loop here since sc can spuriously fail
addi t2, t0, 1
sc.d t1, t2, ({state})
# loop if we failed
bnez t1, 0b
mv {error}, x0
j 2f

1:
li {error}, 1
2:
                ",
		out("t0") old,
		out("t1") _,
		out("t2") _,
		halfway = in(reg) u32::MAX as u64,
		error = out(reg) error,
		state = in(reg) self.state.as_ptr(),
		options(nostack));
        }

        assert!(
            old != u32::MAX as u64 - 1,
            "SpinRwLock::try_read: too many readers"
        );

        match error {
            0 => {
                // We have acquired the lock.
                Some(SpinRwLockReadGuard {
                    rwlock: self,
                    _phantom: PhantomData,
                })
            }
            1 => {
                // Lock failed: locked mutable
                // Let's see if we're deadlocked.
                assert!(
                    !(old >= u32::MAX as u64 && old == hartid + u32::MAX as u64),
                    "SpinRwLock::try_read: deadlock detected"
                );
                // Make sure we re-enable interrupts
                if hart_local::enabled() {
                    LOCAL_HART.with(hart_local::HartCtx::pop_off);
                }
                None
            }
            _ => unreachable!(),
        }
    }

    /// Try to lock the `SpinRwLock` as readable, blocking if it is
    /// not available.
    ///
    /// # Panics
    ///
    /// This function will panic if the number of readers is
    /// >=`usize::MAX-1`, or if it detects a deadlock.
    #[track_caller]
    pub fn read(&'_ self) -> SpinRwLockReadGuard<'_, T> {
        // Disable interrupts to prevent deadlocks. Interrupts will
        // always be disabled before TLS is enabled, as it's enabled
        // very early (before paging, even)
        let hartid = if hart_local::enabled() {
            LOCAL_HART.with(|hart| {
                hart.push_off();
                hart.hartid.get()
            })
        } else {
            hartid()
        };

        let state = self.state.load(Ordering::Acquire);
        assert!(
            !(state >= u32::MAX as u64 && state == hartid + u32::MAX as u64),
            "SpinRwLock::read: deadlock detected"
        );
        //crate::println!("{:#x?}", state);

        let old: u64;

        // Try to increase the number of readers by one, using an
        // Acquire load-reserved and a Relaxed store-conditional. We
        // go into a short loop until the store-conditional succeeds,
        // but first short-circuit to spin if we're for sure
        // locked. This is essentially a cmpxchg loop, but more easy
        // to reason about and likely much more efficient.
        //
        // SAFETY: This code does not break any of our invariants, and
        // does not cause undefined behaviour.
        unsafe {
            core::arch::asm!(
		"
0:
j 2f
1:
pause
2:
# try to acquire-load state
lr.d.aq t0, ({state})

# if state >= u32::MAX, fail
# (locked mutable)
bge t0, {halfway}, 1b

# try to store-relaxed t0+1
# if s has since changed, this will fail.
# we go into a short loop here since sc can spuriously fail
addi t2, t0, 1
sc.d t1, t2, ({state})
# loop if we failed
# don't pause, this may be a shorter loop
bnez t1, 2b
",
		out("t0") old,
		out("t1") _,
		out("t2") _,
		halfway = in(reg) u32::MAX as u64,
		state = in(reg) self.state.as_ptr(),
		options(nostack));
        }

        assert!(
            old != u32::MAX as u64 - 1,
            "SpinRwLock::try_read: too many readers"
        );

        // We have acquired the lock. If we had too many readers, the
        // above guard caught that. And we would've spun if we were
        // still locked.
        SpinRwLockReadGuard {
            rwlock: self,
            _phantom: PhantomData,
        }
    }

    /// Try to lock the `SpinRwLock` as writable, returning `None` if
    /// it is not available.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    #[track_caller]
    pub fn try_write(&'_ self) -> Option<SpinRwLockWriteGuard<'_, T>> {
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

        // Try to set the state to locked-writable, if it was
        // unlocked.
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
        // or allow aliasing of &T and &mut T, which is
        // mega-unsound. Thus we need to ensure it was **definitely**
        // unlocked when we swap, not just lock it anyway.
        if let Err(e) = self.state.compare_exchange(
            0,
            u32::MAX as u64 + hartid as u64,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            assert!(
                !(e >= u32::MAX as u64 && e == hartid + u32::MAX as u64),
                "SpinRwLock::try_write: deadlock detected"
            );
            // Make sure we re-enable interrupts if we didn't lock.
            if hart_local::enabled() {
                LOCAL_HART.with(hart_local::HartCtx::pop_off);
            }
            return None;
        }

        Some(SpinRwLockWriteGuard {
            rwlock: self,
            _phantom: PhantomData,
        })
    }

    /// Lock the `SpinRwLock`, potentially spinning if it is not available.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    #[track_caller]
    pub fn write(&'_ self) -> SpinRwLockWriteGuard<'_, T> {
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

        // Try to set the locked state to locked-writable, if it was
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
        // or allow aliasing of &T and &mut T, which is
        // mega-unsound. Thus we need to ensure it was **definitely**
        // unlocked when we swap, not just lock it anyway.
        while let Err(e) = self.state.compare_exchange(
            0,
            u32::MAX as u64 + hartid as u64,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            assert!(
                !(e >= u32::MAX as u64 && e == hartid + u32::MAX as u64),
                "SpinRwLock::write: deadlock detected"
            );
            hint::spin_loop();
        }

        SpinRwLockWriteGuard {
            rwlock: self,
            _phantom: PhantomData,
        }
    }
}

impl<T: Default> Default for SpinRwLock<T> {
    fn default() -> Self {
        SpinRwLock::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for SpinRwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(lock) => f.debug_struct("SpinRwLock").field("data", &*lock).finish(),
            None => f
                .debug_struct("SpinRwLock")
                .field("data", &"<locked>")
                .field("held_by", &(self.state.load(Ordering::Relaxed) - u64::MAX))
                .finish_non_exhaustive(),
        }
    }
}

pub struct SpinRwLockReadGuard<'a, T> {
    rwlock: &'a SpinRwLock<T>,
    _phantom: PhantomData<&'a T>,
}

impl<'a, T: 'a> SpinRwLockReadGuard<'a, T> {
    pub fn map<U: ?Sized + 'a>(
        self,
        f: impl FnOnce(&T) -> &U,
    ) -> MappedSpinRwLockReadGuard<'a, T, U> {
        let mapped = f(&*self) as *const _;
        MappedSpinRwLockReadGuard {
            guard: self,
            mapped,
        }
    }

    /// Try to atomically upgrade this guard to a writable guard,
    /// blocking until it can.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    pub fn upgrade(self) -> SpinRwLockWriteGuard<'a, T> {
        // Interrupts are already disabled by way of holding this lock.
        let hartid = hartid();

        // Try to acquire-store the locked guard into the state, if
        // there was only 1 reader (us). Spin if we can't.
        if let Err(e) = self.rwlock.state.compare_exchange(
            1,
            u32::MAX as u64 + hartid,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            assert!(
                !(e >= u32::MAX as u64 && e == hartid + u32::MAX as u64),
                "SpinRwLockReadGuard::upgrade: deadlock detected"
            );
            hint::spin_loop();
        }
        SpinRwLockWriteGuard {
            rwlock: self.rwlock,
            _phantom: PhantomData,
        }
    }
}

impl<'a, T: 'a> Deref for SpinRwLockReadGuard<'a, T> {
    type Target = T;

    fn deref(&'_ self) -> &'_ Self::Target {
        // SAFETY: The data is valid due to our
        // invariants. Additionally, we can return a &'_ T as we have
        // a &'_ Self.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<'a, T: 'a> Drop for SpinRwLockReadGuard<'a, T> {
    fn drop(&mut self) {
        // Make sure, just in case, that interrupts are still off.
        asm::intr_off();

        // Decrease the number of readers by one. The Release ordering
        // ensures that this unlock happens-before any Acquire-load
        // locks.
        self.rwlock.state.fetch_sub(1, Ordering::Release);

        // If interrupts were previously enabled, re-enable them.
        // Interrupts will always be disabled before TLS is enabled,
        // as it's enabled very early (before paging, even).
        if hart_local::enabled() {
            LOCAL_HART.with(hart_local::HartCtx::pop_off);
        }
    }
}

impl<'a, T: fmt::Debug + 'a> fmt::Debug for SpinRwLockReadGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v: &T = self;
        v.fmt(f)
    }
}

pub struct MappedSpinRwLockReadGuard<'a, T, U: ?Sized> {
    guard: SpinRwLockReadGuard<'a, T>,
    mapped: *const U,
}

impl<'a, T: 'a, U: ?Sized + 'a> MappedSpinRwLockReadGuard<'a, T, U> {
    pub fn map<V: ?Sized + 'a>(
        self,
        f: impl FnOnce(&U) -> &V,
    ) -> MappedSpinRwLockReadGuard<'a, T, V> {
        MappedSpinRwLockReadGuard {
            guard: self.guard,
            // SAFETY: By invariants
            mapped: f(unsafe { &*self.mapped }) as *const _,
        }
    }

    /// Try to atomically upgrade this guard to a writable guard,
    /// blocking until it can.
    ///
    /// # Panics
    ///
    /// This function will panic if it detects a deadlock.
    pub fn upgrade(self) -> MappedSpinRwLockWriteGuard<'a, T, U> {
        let guard = self.guard.upgrade();
        MappedSpinRwLockWriteGuard {
            guard,
            // This pointer is valid, as we have just upgraded the
            // guard to mutable, and doing so will not allow the
            // underlying data to be moved or invalidated.
            mapped: self.mapped.cast_mut(),
        }
    }
}

impl<'a, T: 'a, U: ?Sized + 'a> Deref for MappedSpinRwLockReadGuard<'a, T, U> {
    type Target = U;

    fn deref(&self) -> &Self::Target {
        // SAFETY: By invariants
        unsafe { &*self.mapped }
    }
}

impl<'a, T: 'a, U: ?Sized + fmt::Debug + 'a> fmt::Debug for MappedSpinRwLockReadGuard<'a, T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v: &U = self;
        v.fmt(f)
    }
}

pub struct SpinRwLockWriteGuard<'a, T> {
    rwlock: &'a SpinRwLock<T>,
    _phantom: PhantomData<&'a mut T>,
}

impl<'a, T: 'a> SpinRwLockWriteGuard<'a, T> {
    pub fn map<U: ?Sized + 'a>(
        mut self,
        f: impl FnOnce(&mut T) -> &mut U,
    ) -> MappedSpinRwLockWriteGuard<'a, T, U> {
        let mapped = f(&mut *self) as *mut _;
        MappedSpinRwLockWriteGuard {
            guard: self,
            mapped,
        }
    }
}

impl<'a, T: 'a> Deref for SpinRwLockWriteGuard<'a, T> {
    type Target = T;

    fn deref(&'_ self) -> &'_ Self::Target {
        // SAFETY: The data is valid due to our invariants.
        // Additionally, we can return a &'_ T as we have a &'_ Self.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<'a, T: 'a> DerefMut for SpinRwLockWriteGuard<'a, T> {
    fn deref_mut(&'_ mut self) -> &'_ mut Self::Target {
        // SAFETY: The data is valid due to our invariants.
        // Additionally, we can return a &'_ mut T as we have a &'_ mut Self.
        unsafe { &mut *self.rwlock.data.get() }
    }
}

impl<'a, T: 'a> Drop for SpinRwLockWriteGuard<'a, T> {
    fn drop(&mut self) {
        // Make sure, just in case, that interrupts are still off.
        asm::intr_off();

        // Set the lock status to unlocked. The Release ordering
        // ensures that this unlock happens-before any Acquire-load
        // locks.
        self.rwlock.state.store(0, Ordering::Release);

        // If interrupts were previously enabled, re-enable them.
        // Interrupts will always be disabled before TLS is enabled,
        // as it's enabled very early (before paging, even).
        if hart_local::enabled() {
            LOCAL_HART.with(hart_local::HartCtx::pop_off);
        }
    }
}

impl<'a, T: fmt::Debug + 'a> fmt::Debug for SpinRwLockWriteGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v: &T = self;
        v.fmt(f)
    }
}

pub struct MappedSpinRwLockWriteGuard<'a, T, U: ?Sized> {
    guard: SpinRwLockWriteGuard<'a, T>,
    mapped: *mut U,
}

impl<'a, T: 'a, U: ?Sized + 'a> MappedSpinRwLockWriteGuard<'a, T, U> {
    pub fn map<V: 'a>(
        self,
        f: impl FnOnce(&mut U) -> &mut V,
    ) -> MappedSpinRwLockWriteGuard<'a, T, V> {
        MappedSpinRwLockWriteGuard {
            guard: self.guard,
            // SAFETY: By invariants
            mapped: f(unsafe { &mut *self.mapped }) as *mut _,
        }
    }
}

impl<'a, T: 'a, U: ?Sized + 'a> Deref for MappedSpinRwLockWriteGuard<'a, T, U> {
    type Target = U;

    fn deref(&self) -> &Self::Target {
        // SAFETY: By invariants
        unsafe { &*self.mapped }
    }
}

impl<'a, T: 'a, U: ?Sized + 'a> DerefMut for MappedSpinRwLockWriteGuard<'a, T, U> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: By invariants
        unsafe { &mut *self.mapped }
    }
}

impl<'a, T: 'a, U: ?Sized + fmt::Debug + 'a> fmt::Debug for MappedSpinRwLockWriteGuard<'a, T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v: &U = self;
        v.fmt(f)
    }
}
