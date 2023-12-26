#![allow(clippy::missing_panics_doc)]
use core::sync::atomic::AtomicU64;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::{Arc, Weak},
};
use atomic::Ordering;
use rille::{
    addr::{Kernel, VirtualConst},
    capability::paging::PageSize,
    symbol,
    units::StorageUnits,
};

use crate::{
    asm::{self, hartid, InterruptDisabler},
    capability::{HartMask, Job, Thread, ThreadPriorities, ThreadProtected, ThreadState},
    hart_local::LOCAL_HART,
    kalloc::phys::PMAlloc,
    paging::{PageTableFlags, PagingAllocator, SharedPageTable},
    proc::Context,
    sync::{OnceCell, SpinMutex},
    N_HARTS,
};

#[derive(Debug)]
pub struct Scheduler {
    per_hart: OnceCell<BTreeMap<u64, SpinMutex<SchedulerInner>>>,
}

impl Scheduler {
    pub fn debug() {
        crate::println!("{:#?}", SCHED.per_hart.expect("aa"));
    }
}

static IDLE_BITMAP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct SchedulerInner {
    queues: [VecDeque<Arc<Thread>>; 32],
    idle_thread: Arc<Thread>,
}

impl Default for SchedulerInner {
    fn default() -> Self {
        let idle_thread = Thread::new(
            String::from("<kernel:idle>"),
            Some(SharedPageTable::from_inner(
                // SAFETY: A zeroed page table is valid.
                unsafe { Box::new_zeroed_in(PagingAllocator).assume_init() },
            )),
            Job::new(None).unwrap(),
        );

        let pg = {
            let mut pma = PMAlloc::get();
            pma.allocate(0).unwrap()
        };
        {
            // SAFETY: We have just allocated this page.
            let s = unsafe { core::slice::from_raw_parts_mut(pg.into_virt().into_ptr_mut(), 8) };
            s.copy_from_slice(&[
                0x0f, 0x00, 0x00, 0x01, // wfi
                0x6f, 0xf0, 0xdf, 0xff, // j -4
            ]);
        }
        idle_thread.setup_page_table();
        let trampoline_virt =
            VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());

        idle_thread.private.lock().root_pgtbl.as_mut().unwrap().map(
            None,
            trampoline_virt.into_phys().into_identity(),
            VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
            PageTableFlags::VAD | PageTableFlags::RX,
            PageSize::Base,
        );

        idle_thread.private.lock().root_pgtbl.as_mut().unwrap().map(
            // Idle thread won't be deallocated.
            None,
            pg.into_const().into_identity(),
            VirtualConst::from_usize(0x1000_0000),
            PageTableFlags::USER | PageTableFlags::RX | PageTableFlags::VAD,
            PageSize::Base,
        );
        idle_thread.private.lock().prio = ThreadPriorities {
            base_prio: -1,
            inherited_prio: -1,
            prio_boost: 0,
        };
        idle_thread.trapframe.lock().user_epc = 0x1000_0000;
        idle_thread.private.lock().state = ThreadState::Runnable;
        Self {
            queues: Default::default(),
            idle_thread,
        }
    }
}

static SCHED: Scheduler = Scheduler {
    per_hart: OnceCell::new(),
};

impl Scheduler {
    pub fn init(hart_map: BTreeMap<u64, SpinMutex<SchedulerInner>>) {
        SCHED.per_hart.get_or_init(|| hart_map);
    }
}

impl SchedulerInner {
    fn find_highest_prio(&mut self) -> Option<Arc<Thread>> {
        let mut x = None;
        for queue in self.queues.iter_mut().rev() {
            if let Some(thread) = queue.front().cloned() {
                queue.rotate_left(1);
                x = Some(thread.clone());
                break;
            }
        }
        x
    }
}

// TODO: bitmap
impl Scheduler {
    /// # Safety
    ///
    /// This function must be called only once per hart.
    #[allow(unused_assignments)]
    pub unsafe fn start() -> ! {
        let hartid = LOCAL_HART.hartid.get();
        // Avoid deadlock, make sure this core can interrupt. N.B. the
        // scheduler/interrupt code will never put this scheduler on a
        // different hart--only user code and process kernel code can
        // be rescheduled.
        asm::intr_on();
        'outer: loop {
            core::hint::spin_loop();
            let mut scheduler = SCHED
                .per_hart
                .expect("Scheduler::per_hart")
                .get(&hartid)
                .unwrap()
                .lock();
            loop {
                //crate::println!("DEBUG\n{:#?}\n========\n\n", scheduler);
                let thread = if let Some(thread) = scheduler.find_highest_prio() {
                    thread
                } else {
                    //crate::println!("{:#?}", scheduler);
                    IDLE_BITMAP.fetch_or(1 << hartid, Ordering::Release);
                    scheduler.idle_thread.clone()
                };

                let Some(mut private) = thread.private.try_lock() else {
                    continue 'outer;
                };

                let old_prio = private.prio.effective();

                match private.state {
                    ThreadState::Runnable => {
                        private.state = ThreadState::Running;
                        drop(private);
                        drop(scheduler);

                        {
                            let mut intr = InterruptDisabler::new();
                            {
                                *LOCAL_HART.thread.borrow_mut() = Some(thread);
                            }
                            {
                                let proc = LOCAL_HART.thread.borrow_mut().clone();
                                let thread = proc.unwrap();
                                {
                                    thread.private.lock().hartid = hartid;
                                }

                                drop(intr);
                                LOCAL_HART.intena.set(true);
                                // SAFETY: Our caller guarantees these contexts are valid.
                                unsafe { Context::switch(&thread.context, &LOCAL_HART.context) }
                                intr = InterruptDisabler::new();

                                // let mut private = thread.private.lock();
                                // let state = private.state;
                                // // We might've been rescheduled in the meantime.
                                // let old_hartid = private.hartid;
                                // let new_hartid = Scheduler::choose_hart_dl(&thread, &mut private);
                                // let new_prio = private.prio.effective();
                                // if new_prio >= 0
                                //     && hartid != new_hartid
                                //     && state != ThreadState::Suspended
                                //     && state != ThreadState::Blocking
                                // {
                                //     // We won't deadlock here cause we dropped `scheduler` above.
                                //     let per_hart = SCHED.per_hart.expect("sched");
                                //     let mut old_hart = per_hart.get(&hartid).unwrap().lock();
                                //     let mut new_hart = per_hart.get(&new_hartid).unwrap().lock();
                                //     //let new_prio = thread.prio.effective();
                                //     crate::println!(
                                //         "cur {:#?}, old {:#?}, new {:#?}, {:#?} -> {:#?}",
                                //         hartid,
                                //         old_hartid,
                                //         new_hartid,
                                //         old_prio,
                                //         new_prio
                                //     );
                                //     crate::println!(
                                //         "old hart = {old_hart:#?}\nnew hart = {new_hart:#?}"
                                //     );
                                //     old_hart.remove_from_run_queue(&thread, old_prio);
                                //     private.hartid = new_hartid;
                                //     if new_prio > old_prio {
                                //         new_hart.insert_in_run_queue_front(&thread, new_prio);
                                //     } else {
                                //         new_hart.insert_in_run_queue_back(&thread, new_prio);
                                //     }
                                // }
                            }

                            {
                                *LOCAL_HART.thread.borrow_mut() = None;
                            }
                        }

                        continue 'outer;
                    }
                    ThreadState::Suspended => {
                        scheduler.remove_from_run_queue(&thread, old_prio);
                        continue 'outer;
                    }
                    _ => {}
                }
            }
        }
    }
}

impl SchedulerInner {
    #[allow(clippy::cast_sign_loss)]
    #[track_caller]
    fn remove_from_run_queue(&mut self, thread: &Arc<Thread>, prio_queue: i8) {
        debug_assert!(prio_queue >= 0);
        debug_assert!(
            self.queues[prio_queue as usize]
                .iter()
                .any(|x| Arc::ptr_eq(thread, x)),
            "oops"
        );

        self.queues[prio_queue as usize].retain(|x| !Arc::ptr_eq(thread, x));
    }

    #[allow(clippy::cast_sign_loss)]
    #[track_caller]
    fn insert_in_run_queue_front(&mut self, thread: &Arc<Thread>, prio_queue: i8) {
        debug_assert!(prio_queue >= 0);
        self.queues[prio_queue as usize].push_front(Arc::clone(thread));
        IDLE_BITMAP.fetch_and(!(1 << hartid()), Ordering::Release);
    }

    #[allow(clippy::cast_sign_loss)]
    #[track_caller]
    fn insert_in_run_queue_back(&mut self, thread: &Arc<Thread>, prio_queue: i8) {
        debug_assert!(prio_queue >= 0);
        self.queues[prio_queue as usize].push_back(Arc::clone(thread));

        IDLE_BITMAP.fetch_and(!(1 << hartid()), Ordering::Release);
    }
}

impl Scheduler {
    #[track_caller]
    pub fn dequeue_dl(thread: &Arc<Thread>, private: &mut ThreadProtected) {
        let hart = private.hartid;
        let mut per_hart = SCHED.per_hart.expect("sched").get(&hart).unwrap().lock();

        let prio = private.prio.effective();
        debug_assert!(prio >= 0);

        // crate::println!("before={:#?}", per_hart);
        // log::trace!("{:#?}", per_hart);

        per_hart.remove_from_run_queue(thread, prio);
        // crate::println!("after={:#?}", per_hart);
    }

    pub fn enqueue_front_dl(
        thread: &Arc<Thread>,
        hartid: Option<u64>,
        private: &mut ThreadProtected,
    ) {
        let hart = hartid.unwrap_or_else(|| Scheduler::choose_hart_dl(thread, private));
        let mut per_hart = SCHED.per_hart.expect("sched").get(&hart).unwrap().lock();

        per_hart.insert_in_run_queue_front(thread, private.prio.effective());
    }

    #[track_caller]
    #[allow(clippy::cast_sign_loss)]
    pub fn enqueue_dl(thread: &Arc<Thread>, hartid: Option<u64>, private: &mut ThreadProtected) {
        let hart = hartid.unwrap_or_else(|| Scheduler::choose_hart_dl(thread, private));
        let mut per_hart = SCHED.per_hart.expect("sched").get(&hart).unwrap().lock();
        let prio = private.prio.effective();
        debug_assert!(
            !per_hart.queues[prio as usize]
                .iter()
                .any(|x| Arc::ptr_eq(thread, x)),
            "oops"
        );

        per_hart.insert_in_run_queue_back(thread, prio);
    }

    pub fn requeue_local(thread: &Arc<Thread>, old_prio: i8, hartid: u64) {
        Self::requeue_local_dl(thread, old_prio, hartid, &mut thread.private.lock());
    }

    pub fn requeue_local_dl(
        thread: &Arc<Thread>,
        old_prio: i8,
        hartid: u64,
        private: &mut ThreadProtected,
    ) {
        let mut per_hart = SCHED.per_hart.expect("sched").get(&hartid).unwrap().lock();

        let new_prio = private.prio.effective();

        if new_prio == old_prio {
            return;
        }
        if private.state == ThreadState::Runnable {
            per_hart.remove_from_run_queue(thread, old_prio);

            if new_prio > old_prio {
                per_hart.insert_in_run_queue_front(thread, new_prio);
            } else {
                per_hart.insert_in_run_queue_back(thread, new_prio);
            }
        }
    }

    pub fn prio_changed(
        thread: &Arc<Thread>,
        old_prio: i8,
        cur_prio: i8,
        local_resched: &mut bool,
        accum_cpu_mask: &mut HartMask,
        propagate: bool,
    ) {
        let private = thread.private.lock();
        let hartid = private.hartid;
        match private.state {
            ThreadState::Running => {
                if cur_prio < old_prio {
                    if asm::hartid() == hartid {
                        *local_resched = true;
                    } else {
                        accum_cpu_mask.0 |= 1 << hartid;
                    }
                }
            }
            ThreadState::Runnable => {
                let mut sched = SCHED
                    .per_hart
                    .expect("Scheduler::per_hart")
                    .get(&hartid)
                    .unwrap()
                    .lock();
                sched.remove_from_run_queue(thread, old_prio);

                if cur_prio > old_prio {
                    sched.insert_in_run_queue_front(thread, cur_prio);

                    if asm::hartid() == hartid {
                        *local_resched = true;
                    } else {
                        accum_cpu_mask.0 |= 1 << hartid;
                    }
                } else {
                    sched.insert_in_run_queue_back(thread, cur_prio);
                }
            }
            ThreadState::Blocking => {
                if thread
                    .blocked_on_wq
                    .lock()
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .is_some()
                {
                    thread.wait_queue_priority_changed(old_prio, cur_prio, propagate);
                }
            }
            _ => {}
        }
    }

    #[track_caller]
    pub fn choose_hart_dl(thread: &Arc<Thread>, private: &mut ThreadProtected) -> u64 {
        let cur_hart = asm::hartid();
        let cur_hart_mask = 1 << cur_hart;
        let affinity = *thread.affinity.lock();
        let last_hart = private.hartid;
        let last_hart = if last_hart == u64::MAX {
            cur_hart
        } else {
            last_hart
        };
        let last_hart_mask = 1 << last_hart;

        if affinity.0 & cur_hart_mask != 0 && Scheduler::is_idle(cur_hart) {
            return cur_hart;
        }

        if affinity.0 & last_hart_mask != 0 && Scheduler::is_idle(last_hart) {
            return last_hart;
        }

        if let Some(hart) = HartMask(affinity.0 & IDLE_BITMAP.load(Ordering::Acquire))
            .harts()
            .next()
        {
            return hart;
        }

        if affinity.0 & last_hart_mask != 0 {
            return last_hart;
        }

        cur_hart
    }

    pub fn choose_hart(thread: &Arc<Thread>) -> u64 {
        Self::choose_hart_dl(thread, &mut thread.private.lock())
    }

    pub fn is_idle(hartid: u64) -> bool {
        IDLE_BITMAP.load(Ordering::Acquire) & (1 << hartid) != 0
    }
}

impl HartMask {
    pub fn harts(self) -> impl Iterator<Item = u64> {
        (0..N_HARTS.load(Ordering::Relaxed) as u64).filter(move |x| self.0 & (1 << x) != 0)
    }
}
