use core::cell::UnsafeCell;

use crate::spin::SpinMutex;

// TODO: i literally have access to nightly so why am i writing my own
// primitives think it might be cause core's OnceCell is !Sync? i already have a
// lock here so i might as well just do a SpinMutex<OnceCell<T>>?
#[derive(Debug)]
pub struct OnceCell<T: 'static> {
    lock: SpinMutex<()>,
    data: UnsafeCell<Option<T>>,
}

impl<T: 'static> OnceCell<T> {
    pub const fn new() -> Self {
        Self {
            lock: SpinMutex::new(()),
            data: UnsafeCell::new(None),
        }
    }

    pub fn get_or_init(&'static self, f: impl FnOnce() -> T) -> &'static T {
        // Make sure we wait for anyone else trying to initialize first.
        // This lock should never be held for long, unless `f` runs for long.
        let _lock = self.lock.lock();
        // SAFETY: No one else will be mutating the data at this time,
        // as we hold the lock.
        if let Some(data) = unsafe { &*self.data.get() }.as_ref() {
            data
        } else {
            // SAFETY: We have exclusive access to mutate the inner data.
            {
                let data = unsafe { &mut *self.data.get() };
                *data = Some(f());
            }
            // SAFETY: We have just exclusively initialied the data,
            // and no references to it remain.
            let data = unsafe { &*self.data.get() };
            // SAFETY: We know the data is initialized.
            unsafe { data.as_ref().unwrap_unchecked() }
        }
    }
}

unsafe impl<T: Sync> Sync for OnceCell<T> {}
