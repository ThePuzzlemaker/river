use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use crate::{asm, hart_local::LOCAL_HART, once_cell::OnceCell, spin::SpinMutex};

use super::{Context, Proc, ProcState};

static SCHED: Scheduler = Scheduler {
    per_hart: OnceCell::new(),
    wait_queue: SpinMutex::new(BTreeMap::new()),
};

#[derive(Debug)]
pub struct Scheduler {
    per_hart: OnceCell<BTreeMap<u64, SpinMutex<SchedulerInner>>>,
    wait_queue: SpinMutex<BTreeMap<usize, Arc<Proc>>>,
}

// TODO: maybe make this a LocalScheduler and be able to get it via
// the current hart's ctx?
#[derive(Debug)]
struct SchedulerInner {
    procs: VecDeque<usize>,
    run_queue: BTreeMap<usize, Arc<Proc>>,
}

impl Scheduler {
    /// Start the scheduler on the current hart.
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
    pub unsafe fn start() -> ! {
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

    pub fn init() {
        LOCAL_HART.with(|hart| {
            SCHED.per_hart.get_or_init(|| {
                BTreeMap::from([(
                    hart.hartid.get(),
                    SpinMutex::new(SchedulerInner {
                        procs: VecDeque::new(),
                        run_queue: BTreeMap::new(),
                    }),
                )])
            });
        });
    }

    /// Enqueue a process on to the scheduler.
    ///
    /// # Panic
    ///
    /// This function will panic if the scheduler is not initialized.
    pub fn enqueue(proc: Proc) {
        LOCAL_HART.with(|hart| {
            let mut sched = SCHED
                .per_hart
                .expect("Scheduler::enqueue: scheduler not initialized")
                .get(&hart.hartid.get())
                .unwrap()
                .lock();

            let pid = proc.pid;
            sched.run_queue.insert(pid, Arc::new(proc));
            sched.procs.push_back(pid);
        });
    }
}
