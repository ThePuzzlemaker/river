use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use atomic::Ordering;

use crate::{
    asm,
    capability::{Thread, ThreadState},
    hart_local::LOCAL_HART,
    sync::{OnceCell, SpinMutex},
};

use super::Context;

static SCHED: Scheduler = Scheduler {
    per_hart: OnceCell::new(),
    wait_queue: SpinMutex::new(BTreeMap::new()),
};

#[derive(Debug)]
pub struct Scheduler {
    per_hart: OnceCell<BTreeMap<u64, SpinMutex<SchedulerInner>>>,
    wait_queue: SpinMutex<BTreeMap<usize, Arc<Thread>>>,
}

// TODO: maybe make this a LocalScheduler and be able to get it via
// the current hart's ctx?
#[derive(Debug, Default)]
pub struct SchedulerInner {
    procs: VecDeque<usize>,
    run_queue: BTreeMap<usize, Arc<Thread>>,
}

impl Scheduler {
    /// Start the scheduler on the current hart.
    ///
    /// # Safety
    ///
    /// This function must be run only ONCE on the current hart. To
    /// switch to the scheduler kernel thread, use
    /// [`ProcToken::yield_to_scheduler`][1].
    ///
    /// Additionally, process contexts must be valid.
    ///
    /// # Panics
    ///
    /// This function will panic if the scheduler is not initialized.
    ///
    /// [1]: crate::proc::ProcToken::yield_to_scheduler
    pub unsafe fn start() -> ! {
        let hartid = LOCAL_HART.with(|hart| {
            *hart.thread.borrow_mut() = None;
            hart.hartid.get()
        });
        // Avoid deadlock, make sure this core can interrupt. N.B. the
        // scheduler/interrupt code will never put this scheduler on a
        // different hart--only user code and process kernel code can be
        // rescheduled.
        asm::intr_on();
        'outer: loop {
            core::hint::spin_loop();
            let mut scheduler = SCHED
                .per_hart
                .expect("Scheduler::per_hart")
                .get(&hartid)
                .unwrap()
                .lock();
            // crate::println!("{:#?}", scheduler);
            loop {
                let (_pid, proc) = if let Some(&pid) = scheduler.procs.front() {
                    scheduler.procs.rotate_left(1);
                    (pid, Arc::clone(scheduler.run_queue.get(&pid).unwrap()))
                } else {
                    //continue 'outer;
                    let mut wait_queue = SCHED.wait_queue.lock();
                    if wait_queue.is_empty() {
                        continue 'outer;
                    }

                    let v = (asm::read_time() as usize * 717) % wait_queue.len();
                    if let Some(entry) = wait_queue.values().nth(v).cloned() {
                        if entry
                            .state
                            .compare_exchange(
                                ThreadState::Runnable,
                                ThreadState::Running,
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                            )
                            .is_ok()
                        {
                            // crate::info!(
                            //     "adopting thread with tid={} from the wait queue",
                            //     entry.tid
                            // );
                            let (pid, proc) = wait_queue.remove_entry(&entry.tid).unwrap();
                            scheduler.run_queue.insert(pid, Arc::clone(&proc));
                            scheduler.procs.push_back(pid);
                            proc.state.store(ThreadState::Runnable, Ordering::Relaxed);
                            (pid, proc)
                        } else {
                            continue 'outer;
                        }
                    } else {
                        continue 'outer;
                    }
                };

                match proc.state.compare_exchange(
                    ThreadState::Runnable,
                    ThreadState::Running,
                    Ordering::Relaxed,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        drop(scheduler);

                        LOCAL_HART.with(move |hart| {
                            *hart.thread.borrow_mut() = Some(proc);

                            {
                                let proc = hart.thread.borrow();
                                let proc = proc.as_ref().unwrap();
                                proc.hartid.store(hart.hartid.get(), Ordering::Relaxed);

                                // SAFETY: Our caller guarantees these contexts are valid.
                                unsafe { Context::switch(&proc.context, &hart.context) }
                            }

                            *hart.thread.borrow_mut() = None;
                        });

                        continue 'outer;
                    }
                    Err(ThreadState::Suspended) => {
                        scheduler.run_queue.remove(&proc.tid);
                        scheduler.procs.pop_back();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Dequeue a process from a hart's scheduler.
    ///
    /// # Panics
    ///
    /// This function will panic if the scheduler is not initialized.
    pub fn dequeue(hartid: u64, tid: usize) {
        let mut sched = SCHED
            .per_hart
            .expect("Scheduler::dequeue: scheduler not initialized")
            .get(&hartid)
            .unwrap()
            .lock();

        sched.run_queue.remove(&tid).unwrap();
        sched.procs.retain(|x| *x != tid);
    }

    pub fn init(hart_map: BTreeMap<u64, SpinMutex<SchedulerInner>>) {
        LOCAL_HART.with(|_hart| {
            SCHED.per_hart.get_or_init(|| hart_map);
        });
    }

    /// Enqueue a process on to the scheduler.
    ///
    /// # Panics
    ///
    /// This function will panic if the scheduler is not initialized.
    pub fn enqueue(proc: Arc<Thread>) {
        LOCAL_HART.with(|hart| {
            let mut sched = SCHED
                .per_hart
                .expect("Scheduler::enqueue: scheduler not initialized")
                .get(&hart.hartid.get())
                .unwrap()
                .lock();

            let pid = proc.tid;
            sched.run_queue.insert(pid, proc);
            sched.procs.push_back(pid);
        });
    }

    /// Move the current process to the wait queue.
    ///
    /// # Panics
    ///
    /// This function will panic if the scheduler is not initialized,
    /// or if there is no process currently running.
    pub fn current_proc_wait() {
        LOCAL_HART.with(|hart| {
            let mut sched = SCHED
                .per_hart
                .expect("Scheduler::current_proc_wait: scheduler not initialized")
                .get(&hart.hartid.get())
                .unwrap()
                .lock();

            let proc = hart
                .thread
                .borrow()
                .as_ref()
                .cloned()
                .expect("Scheduler::current_proc_wait: no process");
            let pid = proc.tid;

            sched.run_queue.remove(&pid);
            sched.procs.retain(|p| *p != pid);

            let mut wait_queue = SCHED.wait_queue.lock();
            wait_queue.insert(pid, proc);
        });
    }

    pub fn wake_up_on(channel: u64) {
        let wait_queue = SCHED.wait_queue.lock();

        for thread in wait_queue.values() {
            if thread
                .waiting_on
                .compare_exchange(channel, 0, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                thread.state.store(ThreadState::Runnable, Ordering::Release);
                break;
            }
        }
    }
}
