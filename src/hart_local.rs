use core::{
    cell::{Cell, RefCell},
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

#[doc(hidden)]
#[repr(transparent)]
pub struct UnsafeSendSync<T: 'static>(pub T);

unsafe impl<T: 'static> Send for UnsafeSendSync<T> {}
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

    #[track_caller]
    pub fn with<R: 'static>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        // Ensure the offset is valid.
        if !self.offset_init.get() {
            let fnptr = self.base_func.take().unwrap();
            // Make sure the fn pointer actually points somewhere
            let fnptr = if !paging::enabled() {
                let fnptr = fnptr as usize;
                let fnptr: VirtualConst<u8, Kernel> = VirtualConst::from_usize(fnptr);
                unsafe {
                    mem::transmute::<usize, unsafe fn() -> usize>(fnptr.into_phys().into_usize())
                }
            } else {
                fnptr
            };
            let base = unsafe { fnptr() };
            self.offset.set(base - symbol::tdata_start().into_usize());
            self.offset_init.set(true);
        }

        let tp = asm::tp();
        // The true data will never be below the kernel.
        if tp < KERNEL_PHYS_OFFSET as u64 {
            panic!(
                "HartLocal::with: attempted to use hart-local data before they were initialized"
            );
        }
        // SAFETY: By our creation, we are guaranteed that our offset is valid and unique.
        // Additionally, the thread-local data is guaranteed to be 'static.
        let val: &'static mut MaybeUninit<T> =
            unsafe { &mut *(tp as *mut u8).add(self.offset.get()).cast() };
        if self.init_func.get().is_some() {
            let init_func = self.init_func.take().unwrap();
            // Make sure the fn pointer actually points somewhere
            let init_func = if !paging::enabled() {
                let init_func = init_func as usize;
                let init_func: VirtualConst<u8, Kernel> = VirtualConst::from_usize(init_func);
                unsafe { mem::transmute::<usize, fn() -> T>(init_func.into_phys().into_usize()) }
            } else {
                init_func
            };
            val.write(init_func());
        }
        // SAFETY: We have just initialized this, or it was previously.
        let inner = unsafe { val.assume_init_ref() };
        f(inner)
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

    let old_data = unsafe { slice::from_raw_parts(tdata_start().as_ptr_mut(), size) };
    let new_ptr = {
        let mut pma = PMAlloc::get();
        pma.allocate(phys::what_order(size))
    }
    .expect("hart_local::init: failed to allocate hart-local data")
    .into_virt();

    unsafe { slice::from_raw_parts_mut(new_ptr.into_ptr_mut(), size).copy_from_slice(old_data) };

    let hartid = unsafe { asm::set_tp(new_ptr) };
    LOCAL_HART.with(|hart| hart.borrow_mut().hartid = hartid);
}

pub fn enabled() -> bool {
    // It's unlikely there's going to be over 0x8020_0000 harts.
    // Anything after this is most definitely an address.
    asm::tp() > KERNEL_PHYS_OFFSET as u64
}

hart_local! {
    pub static LOCAL_HART: RefCell<HartCtx> = RefCell::new(HartCtx::default())
}

#[derive(Clone, Debug, Default)]
pub struct HartCtx {
    /// Number of .push_off()s done without a .pop_off()
    pub n_introff: usize,
    /// Were interrupts enabled before doing .push_off?
    pub intena: bool,
    /// The hart ID
    pub hartid: u64,
}

impl HartCtx {
    pub fn push_off(&mut self) {
        // If we're the first one to push_off, disable interrupts
        // and mark down intena.
        if self.n_introff == 0 {
            self.intena = intr_off();
        }
        // Increment the count
        self.n_introff += 1;
    }

    pub fn pop_off(&mut self) {
        // Decrement the count
        self.n_introff -= 1;

        // If we were the last one to pop_off, and interrupts
        // were previously enabled, re-enable them.
        if self.n_introff == 0 && self.intena {
            intr_on();
        }
    }
}
