use core::{
    arch::global_asm,
    cell::{RefCell, RefMut},
    mem::{self, MaybeUninit},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::Arc,
};

use crate::{
    addr::{DirectMapped, Kernel, VirtualConst, VirtualMut},
    asm,
    hart_local::{HartCtx, LOCAL_HART},
    kalloc::phys::{self, PMAlloc},
    once_cell::OnceCell,
    paging::{PageTable, PageTableFlags},
    spin::SpinMutex,
    symbol,
    trampoline::Trapframe,
    trap::user_trap_ret,
    units::StorageUnits,
};

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

/// Register spill area for kernel context switches.
///
/// N.B. We don't need to store tx and ax registers, as we are in the
/// kernel when these are swapped, so our we only must save
/// callee-saved registers (sx)
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Context {
    /*   0 */ pub ra: u64,
    /*   8 */ pub sp: u64,
    /*  16 */ pub s0: u64,
    /*  24 */ pub s1: u64,
    /*  32 */ pub s2: u64,
    /*  40 */ pub s3: u64,
    /*  48 */ pub s4: u64,
    /*  56 */ pub s5: u64,
    /*  64 */ pub s6: u64,
    /*  72 */ pub s7: u64,
    /*  80 */ pub s8: u64,
    /*  88 */ pub s9: u64,
    /*  96 */ pub s10: u64,
    /* 104 */ pub s11: u64,
}

global_asm!(
    "
.pushsection .text
.globl context_switch
.type context_switch, @function
context_switch:
    sd ra,    0(a0)
    sd sp,    8(a0)
    sd s0,   16(a0)
    sd s1,   24(a0)
    sd s2,   32(a0)
    sd s3,   40(a0)
    sd s4,   48(a0)
    sd s5,   56(a0)
    sd s6,   64(a0)
    sd s7,   72(a0)
    sd s8,   80(a0)
    sd s9,   88(a0)
    sd s10,  96(a0)
    sd s11, 104(a0)

    sd zero, 0(a2)
    fence

    ld ra,    0(a1)
    ld sp,    8(a1)
    ld s0,   16(a1)
    ld s1,   24(a1)
    ld s2,   32(a1)
    ld s3,   40(a1)
    ld s4,   48(a1)
    ld s5,   56(a1)
    ld s6,   64(a1)
    ld s7,   72(a1)
    ld s8,   80(a1)
    ld s9,   88(a1)
    ld s10,  96(a1)
    ld s11, 104(a1)

    sd zero, 0(a3)
    fence

    ret
.popsection
"
);

extern "C" {
    pub fn context_switch(
        old: *mut Context,
        new: *mut Context,
        old_lock: *const AtomicU64,
        new_lock: *const AtomicU64,
    );
}

pub static SCHED: Scheduler = Scheduler {
    per_hart: OnceCell::new(),
    wait_queue: SpinMutex::new(BTreeMap::new()),
};

#[derive(Debug)]
pub struct Scheduler {
    pub per_hart: OnceCell<BTreeMap<u64, SpinMutex<SchedulerInner>>>,
    pub wait_queue: SpinMutex<BTreeMap<usize, Arc<Proc>>>,
}

#[derive(Debug)]
pub struct SchedulerInner {
    pub procs: VecDeque<usize>,
    pub run_queue: BTreeMap<usize, Arc<Proc>>,
}

/// Run the scheduler on the current hart.
///
/// # Safety
///
/// This function must be run only ONCE on the current hart. To switch
/// to the scheduler kernel thread, use [`goto_scheduler`] or [`proc_yield`].
///
/// # Panics
///
/// This function will panic if the scheduler is not initialized.
pub unsafe fn scheduler() -> ! {
    LOCAL_HART.with(|hart| {
        *hart.proc.borrow_mut() = None;
    });
    // Avoid deadlock, make sure this core can interrupt. N.B. the
    // scheduler/interrupt code will never put this scheduler on a
    // different hart--only user code and process kernel code can be
    // rescheduled.
    asm::intr_on();
    'outer: loop {
        let mut scheduler = SCHED
            .per_hart
            .expect("Scheduler::per_hart")
            .get(&asm::hartid())
            .unwrap()
            .lock();
        loop {
            let pid = *scheduler.procs.front().unwrap();
            scheduler.procs.rotate_left(1);
            let proc = Arc::clone(scheduler.run_queue.get(&pid).unwrap());
            let mut proc_lock = proc.spin_protected.lock();
            if proc_lock.state == ProcState::Runnable {
                drop(scheduler);
                proc_lock.state = ProcState::Running;
                drop(proc_lock);

                LOCAL_HART.with(move |hart| {
                    *hart.proc.borrow_mut() = Some(proc);

                    let token = hart.proc().unwrap();

                    let proc = token.proc();

                    // Lock the contexts, but forget the lock
                    // existed, so that they can be used within
                    // `context_switch` but do not get spuriously
                    // unlocked after we've switched back here.
                    mem::forget(hart.context.lock());
                    mem::forget(proc.context.lock());

                    let (old_ctx, old_lock) = hart.context.to_components();
                    let (new_ctx, new_lock) = proc.context.to_components();

                    // SAFETY: Both contexts are valid and locked.
                    unsafe {
                        context_switch(old_ctx, new_ctx, old_lock, new_lock);
                    }

                    *hart.proc.borrow_mut() = None;
                });

                continue 'outer;
            }
        }
    }
}

pub fn proc_yield(token: &ProcToken<'_>) {
    let mut proc_lock = token.proc.spin_protected.lock();
    proc_lock.state = ProcState::Runnable;
    drop(proc_lock);

    goto_scheduler(token);
}

pub fn goto_scheduler(token: &ProcToken) {
    LOCAL_HART.with(|hart| {
        let intena = hart.intena.get();
        let proc = token.proc();

        // Lock the contexts, but forget the lock existed, so that
        // they can be used within `context_switch` but do not get
        // spuriously unlocked after we've switched back here.
        mem::forget(proc.context.lock());
        mem::forget(hart.context.lock());

        let (old_ctx, old_lock) = proc.context.to_components();
        let (new_ctx, new_lock) = hart.context.to_components();

        // SAFETY: Both contexts are valid and locked.
        unsafe { context_switch(old_ctx, new_ctx, old_lock, new_lock) };
        hart.intena.set(intena);
    });
}
