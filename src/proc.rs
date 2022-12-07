use core::{
    cell::{RefCell, RefMut},
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

use alloc::{string::String, sync::Arc};

use crate::{
    addr::{DirectMapped, Kernel, VirtualConst, VirtualMut},
    hart_local::HartCtx,
    kalloc::phys::{self, PMAlloc},
    paging::{PageTable, PageTableFlags},
    spin::SpinMutex,
    symbol,
    trampoline::Trapframe,
    trap::user_trap_ret,
    units::StorageUnits,
};

use self::sched::goto_scheduler;

#[derive(Debug)]
pub struct Proc {
    pub pid: usize,
    pub spin_protected: SpinMutex<ProcProtected>,
    // N.B. We don't need atomics here since there's no way that we
    // can soundly obtain two ProcTokens to the same process on
    // different threads, and the only way to modify (or read) this
    // value is by having a ProcToken.
    pub private: RefCell<ProcPrivate>,
    pub context: SpinMutex<Context>,
}

// SAFETY: The data inside is SpinMutex-protected, or otherwise protected.
unsafe impl Send for Proc {}
// SAFETY: See above.
unsafe impl Sync for Proc {}

#[derive(Debug)]
pub struct ProcPrivate {
    pub kernel_stack: VirtualMut<u8, DirectMapped>,
    pub mem_size: usize,
    pub pgtbl: PageTable,
    pub trapframe: *mut MaybeUninit<Trapframe>,
    pub name: String,
}

static NEXTPID: AtomicUsize = AtomicUsize::new(0);

impl Proc {
    /// # Panics
    /// TODO
    pub fn new(name: String) -> Proc {
        let (kernel_stack, trapframe) = {
            let mut pma = PMAlloc::get();
            let kernel_stack = pma.allocate(phys::what_order(4.mib())).unwrap();
            let trapframe = pma.allocate(phys::what_order(4.kib())).unwrap();
            (kernel_stack, trapframe)
        };
        let mut pgtbl = PageTable::new();
        let trampoline_virt =
            VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());
        pgtbl.map(
            trampoline_virt.into_phys().into_identity(),
            VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
            PageTableFlags::VAD | PageTableFlags::RX,
        );
        pgtbl.map(
            trapframe.into_const().into_identity(),
            VirtualConst::from_usize(usize::MAX - 3 * 4.kib() + 1),
            PageTableFlags::VAD | PageTableFlags::RW,
        );

        let kernel_stack_virt = kernel_stack.into_virt();

        let context = SpinMutex::new(Context {
            ra: user_trap_ret as usize as u64,
            sp: kernel_stack_virt.into_usize() as u64 + 4.mib(),
            ..Context::default()
        });

        Proc {
            // Ordering of this increment does not matter, just
            // atomicity.
            pid: NEXTPID.fetch_add(1, Ordering::Relaxed),
            spin_protected: SpinMutex::new(ProcProtected {
                state: ProcState::Uninit,
            }),
            private: RefCell::new(ProcPrivate {
                kernel_stack: kernel_stack_virt,
                mem_size: 0,
                pgtbl,
                trapframe: trapframe.into_virt().cast().into_ptr_mut(),
                name,
            }),
            context,
        }
    }

    /// Get a mutable reference to the private data of a Proc.
    ///
    /// # Panics
    ///
    /// This function will panic if the private data is already
    /// mutably borrowed.
    #[track_caller]
    // SAFETY: See below comment.
    #[allow(clippy::mut_from_ref)]
    pub fn private<'h>(&'h self, token: &ProcToken<'h>) -> RefMut<'h, ProcPrivate> {
        assert_eq!(
            self.pid, token.proc.pid,
            "Proc::private: token did not refer to the same process"
        );
        self.private.borrow_mut()
    }

    /// Get a mutable reference to the private data of a mutably
    /// borrowed Proc (i.e., one that is still in the initialization
    /// phase).
    pub fn private_mut(&mut self) -> &mut ProcPrivate {
        self.private.get_mut()
    }
}

/// An opaque token representing the process running on the current
/// hart.
#[derive(Debug)]
pub struct ProcToken<'h> {
    proc: Arc<Proc>,
    hart: &'h HartCtx,
}

impl<'h> ProcToken<'h> {
    pub fn proc(&self) -> &Arc<Proc> {
        &self.proc
    }

    pub fn cpu(&self) -> &'h HartCtx {
        self.hart
    }
}

impl HartCtx {
    pub fn proc(&'_ self) -> Option<ProcToken<'_>> {
        let proc = Arc::clone(self.proc.borrow().as_ref()?);
        Some(ProcToken { proc, hart: self })
    }
}

#[derive(Debug)]
pub struct ProcProtected {
    pub state: ProcState,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProcState {
    Uninit,
    Sleeping,
    Runnable,
    Running,
}

mod context;
mod sched;
pub use context::*;
pub use sched::*;

pub fn proc_yield(token: &ProcToken<'_>) {
    let mut proc_lock = token.proc.spin_protected.lock();
    proc_lock.state = ProcState::Runnable;
    drop(proc_lock);

    goto_scheduler(token);
}
