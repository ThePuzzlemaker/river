//! A mutex implementation using [`Notification`]s.
use core::{
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, Ordering},
};

use rille::capability::Notification;

/// A mutex implementation using [`Notification`]s.
#[repr(C)]
pub struct Mutex<T> {
    data: UnsafeCell<T>,
    /// 0: unlocked
    /// 1: locked, no waiters
    /// 2: locked, with waiters
    state: AtomicU32,
    notif: Notification,
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(lock) => f.debug_struct("SpinMutex").field("data", &*lock).finish(),
            None => f
                .debug_struct("SpinMutex")
                .field("data", &"<locked>")
                .finish_non_exhaustive(),
        }
    }
}

// SAFETY: `Mutex`es only provide mutually exclusive access to their
// data, and their locking state is atomic and thus does not provide
// simultaneous mutable access due to race conditions.
unsafe impl<T: Send> Send for Mutex<T> {}
// SAFETY: We only need to have T: Send because we may be able to send
// the T between threads through a `&mut T`, but we can't use the
// Mutex to get a `&T`.
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create a new `Mutex`.
    pub fn new(data: T, notif: Notification) -> Self {
        Self {
            data: UnsafeCell::new(data),
            state: AtomicU32::new(0),
            notif,
        }
    }

    /// Check if the mutex is locked.
    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) != 0
    }

    /// Try to atomically lock the mutex, blocking if it is currently locked.
    pub fn lock(&'_ self) -> MutexGuard<'_, T> {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_contended();
        }
        MutexGuard {
            mutex: self,
            _phantom: PhantomData,
        }
    }

    /// Try to atomically lock the mutex, returning `None` if it is currently locked.
    pub fn try_lock(&'_ self) -> Option<MutexGuard<'_, T>> {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(MutexGuard {
            mutex: self,
            _phantom: PhantomData,
        })
    }

    #[inline]
    #[cold]
    fn lock_contended(&self) {
        let mut spin_count = 0;

        while self.state.load(Ordering::Relaxed) == 1 && spin_count < 100 {
            spin_count += 1;
            core::hint::spin_loop();
        }

        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }

        while self.state.swap(2, Ordering::Acquire) != 0 {
            self.notif
                .wait()
                .expect("RawMutex::lock: notification error");
        }
    }
}

/// A guard over a locked [`Mutex`] that allows access to the inner
/// data.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    _phantom: PhantomData<*const T>,
}

impl<'a, T: fmt::Debug> fmt::Debug for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MutexGuard")
            .field("mutex", &self.mutex)
            .field("data", &**self)
            .finish()
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: By our invariants, we have mutable access to this
        // data.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: By our invariants, we have mutable access to this
        // data.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        if self.mutex.state.swap(0, Ordering::Release) == 2 {
            self.mutex
                .notif
                .signal()
                .expect("RawMutex::unlock: notification error");
        }
    }
}
