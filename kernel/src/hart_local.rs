use core::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    slice,
    sync::atomic::{self, AtomicU16, AtomicUsize, Ordering},
};

use alloc::sync::Arc;

use rille::{
    addr::KERNEL_PHYS_OFFSET,
    symbol::{tbss_end, tbss_start},
};

use crate::{
    asm::{self},
    capability::Thread,
    kalloc::phys::{self, PMAlloc},
    proc::Context,
    symbol::{tdata_end, tdata_start},
    sync::SpinMutex,
    MAX_HARTS,
};

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

#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_U16_0: AtomicU16 = AtomicU16::new(0);
pub static CURRENT_ASID: [AtomicU16; MAX_HARTS] = [ATOMIC_U16_0; MAX_HARTS];

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
