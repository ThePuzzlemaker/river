use core::{
    cell::{Cell, RefCell},
    hint::black_box,
    intrinsics::{likely, unlikely},
    marker::PhantomData,
    mem::{self, MaybeUninit},
    slice,
    sync::atomic::{self, Ordering},
};

use alloc::sync::Arc;

use rille::{
    addr::{Kernel, VirtualConst, KERNEL_PHYS_OFFSET},
    symbol::{tbss_end, tbss_start},
};

use crate::{
    asm::{self},
    capability::Thread,
    kalloc::phys::{self, PMAlloc},
    paging,
    proc::Context,
    symbol::{self, tdata_end, tdata_start},
    sync::SpinMutex,
};

// #[macro_export]
// macro_rules! hart_local {
//     () => {};
//     (#[first] $(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $init:expr$(; $($rest:tt)*)?) => {
//         $(#[$attr])*
//         $vis static $name: $crate::hart_local::HartLocal<$ty> = {
//             #[link_section = ".hart_local.first"]
// 	    #[no_mangle]
// 	    #[export_name = "only one hart local may be marked first"]
//             pub static __STATIC_INNER_FIRST: $crate::hart_local::UnsafeSendSync<::core::mem::MaybeUninit<$ty>> = $crate::hart_local::UnsafeSendSync(::core::mem::MaybeUninit::uninit());
//             fn __init() -> $ty {
//                 $init
//             }
//             unsafe fn __get_base() -> usize {
//                 &__STATIC_INNER_FIRST as *const _ as usize
//             }
//             $crate::hart_local::HartLocal::new(__init, __get_base)
//         };
//         $crate::hart_local! { $($($rest)*)? }
//     };
//     ($(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $init:expr$(; $($rest:tt)*)?) => {
//         $(#[$attr])*
//         $vis static $name: $crate::hart_local::HartLocal<$ty> = {
//             #[link_section = ".hart_local"]
//             pub static __STATIC_INNER: $crate::hart_local::UnsafeSendSync<::core::mem::MaybeUninit<$ty>> = $crate::hart_local::UnsafeSendSync(::core::mem::MaybeUninit::uninit());
//             fn __init() -> $ty {
//                 $init
//             }
//             unsafe fn __get_base() -> usize {
//                 &__STATIC_INNER as *const _ as usize
//             }
//             $crate::hart_local::HartLocal::new(__init, __get_base)
//         };
//         $crate::hart_local! { $($($rest)*)? }
//     };

// }

// /// Implement Send+Sync on a non-Send+Sync value.
// ///
// /// # Safety
// ///
// /// DO NOT USE THIS TYPE. THIS IS NOT SAFE!!! This is only used for
// /// macro purposes.
// #[doc(hidden)]
// #[repr(transparent)]
// pub struct UnsafeSendSync<T: 'static>(pub T);

// // SAFETY: The user of this type guarantees that this is safe.
// unsafe impl<T: 'static> Send for UnsafeSendSync<T> {}
// // SAFETY: The user of this type guarantees that this is safe.
// unsafe impl<T: 'static> Sync for UnsafeSendSync<T> {}

// #[derive(Debug)]
// pub struct HartLocal<T: 'static> {
//     offset_init: Cell<bool>,
//     offset: Cell<usize>,
//     init_func: Cell<Option<fn() -> T>>,
//     base_func: Cell<Option<unsafe fn() -> usize>>,
// }

// // SAFETY: Data inside a HartLocal<T> is never sent or shared across
// // threads, and is initialized for all threads.
// unsafe impl<T: 'static> Send for HartLocal<T> {}
// // SAFETY: See above.
// unsafe impl<T: 'static> Sync for HartLocal<T> {}

// impl<T: 'static> HartLocal<T> {
//     #[doc(hidden)]
//     pub const fn new(f: fn() -> T, base_func: unsafe fn() -> usize) -> Self {
//         Self {
//             offset_init: Cell::new(false),
//             offset: Cell::new(0),
//             init_func: Cell::new(Some(f)),
//             base_func: Cell::new(Some(base_func)),
//         }
//     }

//     /// Run a function in the context of the hart-local data. This
//     /// also ensures that the code within the closure does not get
//     /// interrupted.
//     ///
//     /// # Panics
//     ///
//     /// This function will panic if used before hart-local data is initialized.
//     #[track_caller]
//     pub fn with<R: 'static>(&'static self, f: impl for<'a> FnOnce(&'a T) -> R) -> R {
//         // N.B. We **never** give out pointers to our internal data, so we
//         // instead avoid a race condition by turning off interrupts temporarily.
//         // (Note that we don't want infinite recursion, so we must turn off/on
//         // interrupts manually, instead of using push_off/pop_off).
//         let intena = intr_off();
//         // Ensure the offset is valid.
//         if unlikely(!self.offset_init.get()) {
//             let fnptr = self.base_func.take().unwrap();
//             // Make sure the fn pointer actually points somewhere
//             let fnptr = if likely(paging::enabled()) {
//                 fnptr
//             } else {
//                 let fnptr = fnptr as usize;
//                 let fnptr: VirtualConst<u8, Kernel> = VirtualConst::from_usize(fnptr);
//                 // SAFETY: Presuming our virtual->physical conversions
//                 // are valid, this pointer should actually point to
//                 // what we think it does. (And we've already made sure
//                 // paging wasn't enabled, and multi-hart action does
//                 // not occur until *after* paging has been enabled, so
//                 // no race condition there)
//                 unsafe {
//                     mem::transmute::<usize, unsafe fn() -> usize>(fnptr.into_phys().into_usize())
//                 }
//             };
//             // SAFETY: By our invariants, presuming we got the pointer
//             // properly, this should be safe.
//             let base = unsafe { fnptr() };
//             self.offset.set(base - symbol::tdata_start().into_usize());
//             self.offset_init.set(true);
//         }

//         let tp = asm::tp();
//         // The true data will never be below the kernel.
//         debug_assert!(
//             tp >= KERNEL_PHYS_OFFSET as u64,
//             "HartLocal::with: attempted to use hart-local data before they were initialized"
//         );
//         // SAFETY: By our creation, we are guaranteed that our offset is valid and unique.
//         // Additionally, the thread-local data is guaranteed to be 'static.
//         let val: &'static mut MaybeUninit<T> =
//             unsafe { &mut *(tp as *mut u8).add(self.offset.get()).cast() };
//         if unlikely(self.init_func.get().is_some()) {
//             let init_func = self.init_func.take().unwrap();
//             // Make sure the fn pointer actually points somewhere
//             let init_func = if likely(paging::enabled()) {
//                 init_func
//             } else {
//                 let init_func = init_func as usize;
//                 let init_func: VirtualConst<u8, Kernel> = VirtualConst::from_usize(init_func);
//                 // SAFETY: See comment above the previous mem::transmute for fnptr.
//                 unsafe { mem::transmute::<usize, fn() -> T>(init_func.into_phys().into_usize()) }
//             };
//             val.write(init_func());
//         }
//         // SAFETY: We have just initialized this, or it was previously.
//         let inner = unsafe { val.assume_init_ref() };
//         let res = f(inner);
//         if intena {
//             intr_on();
//         }
//         res
//     }
// }

/// Initialize hart-local structures for this hart.
///
/// # Safety
///
/// This must be initialized only once per hart.
///
/// # Panics
///
/// This function will panic if it fails to allocate the required
/// memory.
#[track_caller]
pub unsafe fn init() {
    let size = (tdata_end().into_usize() - tdata_start().into_usize())
        + (tbss_end().into_usize() - tbss_start().into_usize());

    // SAFETY: The linker guarantees this area is valid.
    let old_data = unsafe {
        slice::from_raw_parts(
            tdata_start().as_ptr_mut(),
            tdata_end().into_usize() - tdata_start().into_usize(),
        )
    };
    let new_ptr_phys = {
        let mut pma = PMAlloc::get();
        pma.allocate(phys::what_order(size))
    }
    .expect("hart_local::init: failed to allocate hart-local data");
    let new_ptr = new_ptr_phys.into_virt().into_identity();

    // SAFETY: Our allocation is of requisite size for this data, and is valid.
    unsafe {
        slice::from_raw_parts_mut(
            new_ptr.into_ptr_mut(),
            tdata_end().into_usize() - tdata_start().into_usize(),
        )
        .copy_from_slice(old_data);
    }

    // SAFETY: The thread pointer we set is valid.
    let hartid = unsafe { asm::set_tp(new_ptr.into_usize()) };
    atomic::compiler_fence(Ordering::SeqCst);
    LOCAL_HART.hartid.set(hartid);
}

#[inline(always)]
pub fn enabled() -> bool {
    // It's unlikely there's going to be over 0x8020_0000 harts.
    // Anything after this is most definitely an address.
    asm::tp() > KERNEL_PHYS_OFFSET as u64
}

// hart_local! {
//     #[first] pub static LOCAL_HART: HartCtx = HartCtx::default();
// }

#[thread_local]
pub static LOCAL_HART: HartCtx = HartCtx::new();

#[derive(Debug, Default)]
#[repr(C)]
pub struct HartCtx {
    /// The hart ID
    pub hartid: Cell<u64>,
    #[cfg(debug_assertions)]
    pub holding_locks: Cell<u64>,
    /// What thread are we running?
    pub thread: RefCell<Option<Arc<Thread>>>,
    /// Register spill area for this hart
    pub context: SpinMutex<Context>,
    /// Interval for timer interrupts
    pub timer_interval: Cell<u64>,
    /// Time base frequency
    pub timebase_freq: Cell<u64>,
    pub holding_disabler: Cell<u64>,
    pub intena: Cell<bool>,
    pub inhibit_intena: Cell<bool>,
    /// Ensure HartCtx is !Send + !Sync
    _phantom: PhantomData<*const ()>,
}

impl HartCtx {
    // Compatibility patch
    #[deprecated]
    pub fn with<'a, R: 'a>(&'a self, f: impl FnOnce(&'a Self) -> R) -> R {
        f(self)
    }

    #[cfg(debug_assertions)]
    pub const fn new() -> Self {
        Self {
            hartid: Cell::new(0),
            thread: RefCell::new(None),
            context: SpinMutex::new(Context::new()),
            timer_interval: Cell::new(0),
            timebase_freq: Cell::new(0),
            holding_disabler: Cell::new(0),
            intena: Cell::new(false),
            holding_locks: Cell::new(0),
            inhibit_intena: Cell::new(false),
            _phantom: PhantomData,
        }
    }

    #[cfg(not(debug_assertions))]
    pub const fn new() -> Self {
        Self {
            hartid: Cell::new(0),
            thread: RefCell::new(None),
            context: SpinMutex::new(Context::new()),
            timer_interval: Cell::new(0),
            holding_disabler: Cell::new(0),
            intena: Cell::new(false),
            timebase_freq: Cell::new(0),
            inhibit_intena: Cell::new(false),
            _phantom: PhantomData,
        }
    }
}
