use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use crate::{asm, hart_local::LOCAL_HART, once_cell::OnceCell, spin::SpinMutex};

use super::{Context, Proc, ProcState, ProcToken};

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
/// Additionally, process contexts must be valid.
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

                    // SAFETY: Our caller guarantees these contexts are valid.
                    unsafe { Context::switch(&proc.context, &hart.context) }

                    *hart.proc.borrow_mut() = None;
                });

                continue 'outer;
            }
        }
    }
}

pub fn goto_scheduler(token: &ProcToken) {
    LOCAL_HART.with(|hart| {
        let intena = hart.intena.get();
        let proc = token.proc();

        // SAFETY: The scheduler context is always valid, as
        // guaranteed by the scheduler.
        unsafe { Context::switch(&hart.context, &proc.context) }

        hart.intena.set(intena);
    });
}
