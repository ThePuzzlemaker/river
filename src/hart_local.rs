use core::{
    cell::Cell,
    marker::PhantomData,
    mem::{self, MaybeUninit},
    slice,
};

use crate::{
    addr::{Kernel, VirtualConst, KERNEL_PHYS_OFFSET},
    asm::{self, intr_off, intr_on},
    kalloc::phys::{self, PMAlloc},
    paging,
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

/// Implement Send+Sync on a non-Send+Sync value.
///
/// # Safety
///
/// DO NOT USE THIS TYPE. This is only used for macro purposes.
#[doc(hidden)]
#[repr(transparent)]
pub struct UnsafeSendSync<T: 'static>(pub T);

// SAFETY: The user of this type guarantees that this is safe.
unsafe impl<T: 'static> Send for UnsafeSendSync<T> {}
// SAFETY: The user of this type guarantees that this is safe.
unsafe impl<T: 'static> Sync for UnsafeSendSync<T> {}

#[derive(Debug)]
pub struct HartLocal<T: 'static> {
    offset_init: Cell<bool>,
    offset: Cell<usize>,
    init_func: Cell<Option<fn() -> T>>,
    base_func: Cell<Option<unsafe fn() -> usize>>,
}

unsafe impl<T: 'static> Send for HartLocal<T> {}
unsafe impl<T: 'static> Sync for HartLocal<T> {}

impl<T: 'static> HartLocal<T> {
    #[doc(hidden)]
    pub const fn new(f: fn() -> T, base_func: unsafe fn() -> usize) -> Self {
        Self {
            offset_init: Cell::new(false),
            offset: Cell::new(0),
            init_func: Cell::new(Some(f)),
            base_func: Cell::new(Some(base_func)),
        }
    }

    /// Run a function in the context of the hart-local data. This
    /// also ensures that the code within the closure does not get
    /// interrupted.
    ///
    /// # Panics
    ///
    /// This function will panic if used before hart-local data is initialized.
    #[track_caller]
    pub fn with<R: 'static>(&'static self, f: impl for<'a> FnOnce(&'a T) -> R) -> R {
        // N.B. We **never** give out pointers to our internal data, so we
        // instead avoid a race condition by turning off interrupts temporarily.
        // (Note that we don't want infinite recursion, so we must turn off/on
        // interrupts manually).
        let intena = intr_off();
        // Ensure the offset is valid.
        if !self.offset_init.get() {
            let fnptr = self.base_func.take().unwrap();
            // Make sure the fn pointer actually points somewhere
            let fnptr = if paging::enabled() {
                fnptr
            } else {
                let fnptr = fnptr as usize;
                let fnptr: VirtualConst<u8, Kernel> = VirtualConst::from_usize(fnptr);
                unsafe {
                    mem::transmute::<usize, unsafe fn() -> usize>(fnptr.into_phys().into_usize())
                }
            };
            let base = unsafe { fnptr() };
            self.offset.set(base - symbol::tdata_start().into_usize());
            self.offset_init.set(true);
        }

        let tp = asm::tp();
        // The true data will never be below the kernel.
        assert!(
            tp >= KERNEL_PHYS_OFFSET as u64,
            "HartLocal::with: attempted to use hart-local data before they were initialized"
        );
        // SAFETY: By our creation, we are guaranteed that our offset is valid and unique.
        // Additionally, the thread-local data is guaranteed to be 'static.
        let val: &'static mut MaybeUninit<T> =
            unsafe { &mut *(tp as *mut u8).add(self.offset.get()).cast() };
        if self.init_func.get().is_some() {
            let init_func = self.init_func.take().unwrap();
            // Make sure the fn pointer actually points somewhere
            let init_func = if paging::enabled() {
                init_func
            } else {
                let init_func = init_func as usize;
                let init_func: VirtualConst<u8, Kernel> = VirtualConst::from_usize(init_func);
                unsafe { mem::transmute::<usize, fn() -> T>(init_func.into_phys().into_usize()) }
            };
            val.write(init_func());
        }
        // SAFETY: We have just initialized this, or it was previously.
        let inner = unsafe { val.assume_init_ref() };
        let res = f(inner);
        if intena {
            intr_on();
        }
        res
    }
}

/// Initialize hart-local structures for this hart.
///
/// # Safety
///
/// This must be initialized only once per hart.
#[track_caller]
pub unsafe fn init() {
    let size = tdata_end().into_usize() - tdata_start().into_usize();

    // SAFETY: The linker guarantees this area is valid.
    let old_data = unsafe { slice::from_raw_parts(tdata_start().as_ptr_mut(), size) };
    let new_ptr_phys = {
        let mut pma = PMAlloc::get();
        pma.allocate(phys::what_order(size))
    }
    .expect("hart_local::init: failed to allocate hart-local data");
    let new_ptr = if paging::enabled() {
        new_ptr_phys.into_virt().into_identity()
    } else {
        new_ptr_phys.into_identity().into_virt()
    };

    // SAFETY: Our allocation is of requisite size for this data, and is valid.
    unsafe { slice::from_raw_parts_mut(new_ptr.into_ptr_mut(), size).copy_from_slice(old_data) };

    // SAFETY: The thread pointer we set is valid.
    let hartid = unsafe { asm::set_tp(new_ptr.into_usize()) };
    LOCAL_HART.with(|hart| hart.hartid.set(hartid));
}

pub fn enabled() -> bool {
    // It's unlikely there's going to be over 0x8020_0000 harts.
    // Anything after this is most definitely an address.
    asm::tp() > KERNEL_PHYS_OFFSET as u64
}

hart_local! {
    pub static LOCAL_HART: HartCtx = HartCtx::default()
}

#[derive(Debug, Default)]
pub struct HartCtx {
    /// Number of .push_off()s done without a .pop_off()
    pub n_introff: Cell<usize>,
    /// Were interrupts enabled before doing .push_off()?
    pub intena: Cell<bool>,
    /// The hart ID
    pub hartid: Cell<u64>,
    /// In trap handler?
    pub trap: Cell<bool>,
    /// Ensure HartCtx is !Send + !Sync
    _phantom: PhantomData<*const ()>,
}

impl HartCtx {
    pub fn push_off(&self) {
        if !self.trap.get() {
            // If we're the first one to push_off, disable interrupts
            // and mark down intena.
            if self.n_introff.get() == 0 {
                self.intena.set(intr_off());
            }
            // Increment the count
            self.n_introff.set(self.n_introff.get() + 1);
        }
    }

    pub fn pop_off(&self) {
        if !self.trap.get() {
            // Decrement the count
            self.n_introff.set(self.n_introff.get() - 1);

            // If we were the last one to pop_off, and interrupts
            // were previously enabled, re-enable them.
            if self.n_introff.get() == 0 && self.intena.get() {
                intr_on();
            }
        }
    }
}
