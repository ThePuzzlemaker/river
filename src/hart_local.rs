use core::{cell::Cell, mem::MaybeUninit, slice};

use crate::{
    asm,
    kalloc::phys::{self, PMAlloc},
    symbol::{self, tdata_end, tdata_start},
};

#[macro_export]
macro_rules! hart_local {
    () => {};
    ($(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $init:expr$(; $($rest:tt)*)?) => {
        $(#[$attr])*
        $vis static $name: $crate::hart_local::HartLocal<$ty> = {
            #[link_section = ".hart_local"]
            pub static __STATIC_INNER: $crate::hart_local::UnsafeSendSync<::core::mem::MaybeUninit<$ty>> = $crate::hart_local::UnsafeSendSync(::core::mem::MaybeUninit::uninit());
            fn __init() -> $ty {
                $init
            }
            unsafe fn __get_base() -> usize {
                &__STATIC_INNER as *const _ as usize
            }
            HartLocal::new(__init, __get_base)
        };
        $crate::hart_local! { $($($rest)*)? }
    }
}

#[doc(hidden)]
#[repr(transparent)]
pub struct UnsafeSendSync<T: 'static>(pub T);

unsafe impl<T: 'static> Send for UnsafeSendSync<T> {}
unsafe impl<T: 'static> Sync for UnsafeSendSync<T> {}

pub struct HartLocal<T: 'static> {
    offset_init: Cell<bool>,
    offset: Cell<usize>,
    init_func: Cell<Option<fn() -> T>>,
    base_func: Cell<Option<unsafe fn() -> usize>>,
}

unsafe impl<T: 'static> Send for HartLocal<T> {}
unsafe impl<T: 'static> Sync for HartLocal<T> {}

impl<T: 'static> HartLocal<T> {
    pub const fn new(f: fn() -> T, base_func: unsafe fn() -> usize) -> Self {
        Self {
            offset_init: Cell::new(false),
            offset: Cell::new(0),
            init_func: Cell::new(Some(f)),
            base_func: Cell::new(Some(base_func)),
        }
    }

    pub fn with<R: 'static>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        // Ensure the offset is valid.
        if !self.offset_init.get() {
            let base = unsafe { (self.base_func.take().unwrap())() };
            self.offset.set(base - symbol::tdata_start().into_usize());
            self.offset_init.set(true);
        }

        let tp = asm::tp();
        // SAFETY: By our creation, we are guaranteed that our offset is valid and unique.
        // Additionally, the thread-local data is guaranteed to be 'static.
        let val: &'static mut MaybeUninit<T> =
            unsafe { &mut *(tp as *mut u8).add(self.offset.get()).cast() };
        if self.init_func.get().is_some() {
            let init_func = self.init_func.take().unwrap();
            val.write(init_func());
        }
        // SAFETY: We have just initialized this, or it was previously.
        let inner = unsafe { val.assume_init_ref() };
        f(inner)
    }
}

/// Initialize hart-local structures for this hart.
/// Returns the hartid
///
/// # Safety
///
/// This must be initialized only once per hart.
#[track_caller]
pub unsafe fn init() -> u64 {
    let size = tdata_end().into_usize() - tdata_start().into_usize();

    let old_data = slice::from_raw_parts(tdata_start().as_ptr_mut(), size);
    let new_ptr = {
        let mut pma = PMAlloc::get();
        pma.allocate(phys::what_order(size))
    }
    .expect("hart_local::init: failed to allocate hart-local data")
    .into_virt();

    slice::from_raw_parts_mut(new_ptr.into_ptr_mut(), size).copy_from_slice(old_data);

    asm::set_tp(new_ptr)
}
