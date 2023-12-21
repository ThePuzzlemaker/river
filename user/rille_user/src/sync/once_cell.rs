//! A thread-safe cell that can be initialized only once.
use core::{
    cell::UnsafeCell,
    hint,
    mem::MaybeUninit,
    sync::atomic::{self, AtomicUsize, Ordering},
};

/// A thread-safe cell that can be initialized only once.
#[derive(Debug)]
pub struct OnceCell<T> {
    state: AtomicUsize,
    data: UnsafeCell<MaybeUninit<T>>,
}

const EMPTY: usize = 0;
const WRITING: usize = 1;
const READY: usize = 2;

impl<T> OnceCell<T> {
    /// Create a new, uninitialized `OnceCell`.
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Try to get the contents of the `OnceCell`, running `f()` to
    /// initialize it if the cell was uninitialized.
    pub fn get_or_init<'a>(&'a self, f: impl FnOnce() -> T) -> &'a T
    where
        T: 'a,
    {
        // Acquire ordering here ensures that any writes to the data
        // and the state happen-before this load.
        match self.state.load(Ordering::Acquire) {
            READY => {
                // SAFETY: We've been initialized and this data will
                // never change.
                return unsafe { (*self.data.get()).assume_init_ref() };
            }
            WRITING => {
                // Someone else is still writing. Do a short spin
                // until they're done. Relaxed is fine here as we will
                // never "downgrade" from READY to WRITING or from
                // WRITING to EMPTY.
                while self.state.load(Ordering::Relaxed) != READY {
                    hint::spin_loop();
                }
                // We only need to synchronize, ensuring that the
                // non-atomic data write went through for us, if we're
                // successful.
                atomic::fence(Ordering::Acquire);
                // SAFETY: We've been initialized and this data will
                // never change.
                return unsafe { (*self.data.get()).assume_init_ref() };
            }
            EMPTY => {
                let data = f();
                // The Acquire ordering will ensure that any Release
                // stores that affect the state will happen-before
                // this change from EMPTY to WRITING. Relaxed is fine
                // for the failure ordering as we have stronger
                // guarantees with fences later as needed.
                match self.state.compare_exchange(
                    EMPTY,
                    WRITING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(EMPTY) => {
                        // SAFETY: We are initializing the data with a
                        // proper value, and we have exclusive access
                        // as no other thread will write when state ==
                        // WRITING.
                        unsafe {
                            (*self.data.get()).write(data);
                        }
                        self.state.store(READY, Ordering::Release);
                        // SAFETY: We've been initialized and this
                        // data will never change.
                        return unsafe { (*self.data.get()).assume_init_ref() };
                    }
                    Err(WRITING) => {
                        // Someone else is still writing. Do a short spin
                        // until they're done. Relaxed is fine here as we will
                        // never "downgrade" from READY to WRITING or from
                        // WRITING to EMPTY.
                        while self.state.load(Ordering::Relaxed) != READY {
                            hint::spin_loop();
                        }
                        // We only need to synchronize, ensuring that
                        // the non-atomic data write actually went
                        // through for us, if we're successful.
                        atomic::fence(Ordering::Acquire);

                        // SAFETY: We've been initialized and this data will
                        // never change.
                        return unsafe { (*self.data.get()).assume_init_ref() };
                    }
                    Err(READY) => {
                        // We need to synchronize to make sure that
                        // the non-atomic write went through for us.
                        atomic::fence(Ordering::Acquire);
                        // SAFETY: We've been initialized and this data will
                        // never change.
                        return unsafe { (*self.data.get()).assume_init_ref() };
                    }
                    // Err(EMPTY) should not happen as we are using
                    // compare_exchange not compare_exchange_weak so
                    // this should not spuriously fail.
                    Ok(_) | Err(_) => unreachable!(),
                }
            }
            _ => unreachable!(),
        }
    }

    /// Initialize the `OnceCell` with a value, but don't return it.
    pub fn init(&self, value: T) {
        self.get_or_init(|| value);
    }

    /// This function will attempt to get the inner data of the `OnceCell`, or
    /// will panic with the given `message`.
    ///
    /// # Panics
    ///
    /// This function will panic if the data within this cell is not
    /// initialized.
    #[track_caller]
    pub fn expect<'a>(&'a self, message: &str) -> &'a T
    where
        T: 'a,
    {
        self.get_or_init(|| panic!("OnceCell::expect: {}", message))
    }
}

// SAFETY: The data within the `OnceCell` is only mutated once, and is
// synchronized. We can share the OnceCell across threads as long as
// the data inside can be shared across threads, and we don't give out
// mutable references.
unsafe impl<T: Sync> Sync for OnceCell<T> {}
// SAFETY: See above. Sending the OnceCell<T> across threads will send
// the data inside, but it won't require the data to be shareable.
unsafe impl<T: Send> Send for OnceCell<T> {}

impl<T> Drop for OnceCell<T> {
    fn drop(&mut self) {
        // As we are the last reference to this data, we will not need
        // to synchronize. We only need to drop the data if we are
        // READY. It is not possible to end up dropping this value
        // while we are still WRITING through sound code, unless we
        // unwind--which we don't (-Cpanic=abort).
        if *self.state.get_mut() == READY {
            // SAFETY: We are initialized and ready to drop.
            unsafe { self.data.get_mut().assume_init_drop() }
        }
    }
}
