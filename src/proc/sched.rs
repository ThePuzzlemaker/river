use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use crate::{asm, hart_local::LOCAL_HART, once_cell::OnceCell, println, spin::SpinMutex};

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
#[derive(Debug, Default)]
pub struct SchedulerInner {
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
            // crate::println!("{:#?}", scheduler);
            loop {
                let (pid, proc) = match scheduler.procs.front() {
                    Some(&pid) => {
                        scheduler.procs.rotate_left(1);
                        (pid, Arc::clone(scheduler.run_queue.get(&pid).unwrap()))
                    }
                    None => {
                        continue 'outer;
                        // let mut wait_queue = SCHED.wait_queue.lock();
                        // if !wait_queue.is_empty() {
                        //     let v = (asm::read_time() as usize * 717) % wait_queue.len();
                        //     if let Some(entry) = wait_queue.values().cloned().nth(v) {
                        //         let (pid, proc) = wait_queue.remove_entry(&entry.pid).unwrap();
                        //         (pid, proc)
                        //     } else {
                        //         continue 'outer;
                        //     }
                        // } else {
                        //     continue 'outer;
                        // }
                    }
                };

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

    pub fn init(hart_map: BTreeMap<u64, SpinMutex<SchedulerInner>>) {
        LOCAL_HART.with(|hart| {
            SCHED.per_hart.get_or_init(|| hart_map);
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

    pub fn current_proc_wait() {
        LOCAL_HART.with(|hart| {
            let mut sched = SCHED
                .per_hart
                .expect("Scheduler::current_proc_wait: scheduler not initialized")
                .get(&hart.hartid.get())
                .unwrap()
                .lock();

            let proc = hart
                .proc()
                .expect("Scheduler::current_proc_wait: no process");
            let pid = proc.proc().pid;

            sched.run_queue.remove(&pid);
            sched.procs.retain(|p| *p != pid);
            // crate::println!("{:#?}", sched);

            let mut wait_queue = SCHED.wait_queue.lock();
            wait_queue.insert(pid, Arc::clone(proc.proc()));
        });
    }
}
