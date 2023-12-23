#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value
)]
use core::{
    cmp,
    mem::{self, MaybeUninit},
    num::NonZeroU64,
    ptr, slice,
    sync::atomic::AtomicU64,
};

use alloc::{
    boxed::Box,
    string::String,
    sync::{Arc, Weak},
};
use atomic::Ordering;
use rille::{
    addr::{Identity, Virtual, VirtualConst},
    capability::{
        self,
        paging::{BasePage, DynLevel, GigaPage, MegaPage, PageTableFlags, PagingLevel},
        AnyCap, CapError, CapResult, CapRights, CapabilityType, MessageHeader, UserRegisters,
    },
    units::StorageUnits,
};

use crate::{
    asm::{self, InterruptDisabler},
    capability::{
        CapToOwned, Endpoint, InterruptHandlerCap, InterruptPoolCap, Job, JobCap, Notification,
        NotificationCap, Page, PageCap, Thread, ThreadProtected, ThreadState, WaitQueue,
    },
    hart_local::LOCAL_HART,
    kalloc::{self, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    plic::PLIC,
    print, println,
    sched::Scheduler,
    sync::{SpinMutex, SpinRwLock},
};

pub fn sys_debug_dump_root(thread: Arc<Thread>, _intr: InterruptDisabler) -> Result<(), CapError> {
    //crate::println!("{:#x?}", thread.job.captbl);
    let (free, total) = {
        let pma = PMAlloc::get();
        (pma.num_free_pages(), pma.num_pages())
    };
    let used = total - free;
    let free = free * 4.kib();
    let used = used * 4.kib();
    let total = total * 4.kib();
    crate::info!(
        "current memory usage: {}B / {}B ({}B free)",
        used,
        total,
        free
    );
    Ok(())
}

pub fn sys_debug_cap_identify(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    index: u64,
) -> Result<CapabilityType, CapError> {
    let index = index as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr_lock = root_hdr.read();

    let slot = root_hdr_lock.get::<AnyCap>(index)?;
    Ok(slot.cap.cap_type())
}

pub fn sys_debug_cap_slot(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    index: u64,
) -> Result<(), CapError> {
    let index = index as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr_lock = root_hdr.read();

    let slot = root_hdr_lock.get::<AnyCap>(index)?;

    println!("{:#x?}", slot);

    Ok(())
}

pub fn sys_debug_print(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    str_ptr: VirtualConst<u8, Identity>,
    str_len: u64,
) {
    //println!("{:#x?}, {:#?}", str_ptr, str_len);
    let str_len = str_len as usize;

    let private = thread.private.lock();

    // assert!(
    //     str_ptr.add(str_len).into_usize() < private.mem_size,
    //     "too long"
    // );

    // SAFETY: All values are valid for u8, and process memory is
    // zeroed and never uninitialized. TODO: make this not disclose
    // kernel memory
    let slice = unsafe {
        slice::from_raw_parts(
            private
                .root_pgtbl
                .as_ref()
                .unwrap()
                .walk(str_ptr)
                .unwrap()
                .0
                .into_virt()
                .into_ptr(),
            str_len,
        )
    };
    //println!("{:?}", slice);

    let s = core::str::from_utf8(slice).unwrap();
    print!("{}", s);
}

pub fn sys_grant(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    from_index: u64,
    rights: CapRights,
    badge: u64,
) -> CapResult<u64> {
    let from_index = from_index as usize;

    let root_hdr = thread.job.captbl.clone();
    let from_index = {
        root_hdr
            .read()
            .get_mut::<AnyCap>(from_index)?
            .to_owned_cap()
    };

    let mut into_table = root_hdr.write();
    let captr = into_table.next_slot();
    let into_index = into_table.get_or_insert_mut(captr)?;

    // We don't need to call copy_hook here since replace handles that
    // for us.
    let mut into = into_index.replace(from_index);
    // TODO: badge validation
    into.set_badge(NonZeroU64::new(badge));
    into.update_rights(rights);

    Ok(captr as u64)
}

pub fn sys_delete(thread: Arc<Thread>, _intr: InterruptDisabler, index: u64) -> CapResult<()> {
    let index = index as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr_lock = root_hdr.read();

    let slot = root_hdr_lock.get_mut::<AnyCap>(index)?;

    let _ = slot.delete();

    Ok(())
}

// pub fn sys_create_object(
//     thread: Arc<Thread>,
//     _intr: InterruptDisabler,
//     cap_type: CapabilityType,
//     size: u64,
// ) -> Result<u64, CapError> {
//     let size = size as u8;

//     let private = thread.private.lock();

//     let root_hdr = &private
//         .captbl
//         .as_ref()
//         .cloned()
//         .ok_or(CapError::NotPresent)?;

//     let mut into_index = root_hdr.write();
//     let captr = into_index.next_slot();

//     match cap_type {
//         CapabilityType::Page => {
//             let slot = into_index.get_or_insert_mut(captr)?;
//             perform_page_alloc(size, slot)?;
//         }
//         CapabilityType::PgTbl => {
//             let slot = into_index.get_or_insert_mut(captr)?;
//             perform_pgtbl_alloc(slot)?;
//         }
//         CapabilityType::Thread => {
//             let slot = into_index.get_or_insert_mut(captr)?;
//             perform_thread_alloc(slot)?;
//         }
//         CapabilityType::Notification => {
//             let slot = into_index.get_or_insert_mut(captr)?;
//             perform_notification_alloc(slot)?;
//         }

//         CapabilityType::Endpoint => {
//             let slot = into_index.get_or_insert_mut(captr)?;
//             perform_endpoint_alloc(slot)?;
//         }

//         _ => return Err(CapError::InvalidOperation),
//     }

//     Ok(captr as u64)
// }

pub fn sys_notification_create(thread: Arc<Thread>, _intr: InterruptDisabler) -> CapResult<u64> {
    let root_hdr = thread.job.captbl.clone();
    let mut root_hdr = root_hdr.write();
    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    let _ = slot.replace(Arc::new(Notification {
        word: AtomicU64::new(0),
        wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
    }));

    Ok(captr as u64)
}

pub fn sys_job_create(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    parent: u64,
) -> CapResult<u64> {
    let parent = parent as usize;

    let root_hdr = thread.job.captbl.clone();
    let parent = {
        let root_hdr = root_hdr.read();
        let job = root_hdr.get::<JobCap>(parent)?;
        job.cap.job().unwrap().clone()
    };

    let mut root_hdr = root_hdr.write();
    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    let _ = slot.replace(Job::new(Some(parent)).ok_or(CapError::InvalidOperation)?);

    Ok(captr as u64)
}

pub fn sys_page_create(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    size_log2: u64,
) -> CapResult<u64> {
    let size_log2 = size_log2 as u8;
    if ![
        BasePage::PAGE_SIZE_LOG2 as u8,
        MegaPage::PAGE_SIZE_LOG2 as u8,
        GigaPage::PAGE_SIZE_LOG2 as u8,
    ]
    .contains(&size_log2)
    {
        return Err(CapError::InvalidSize);
    }

    let root_hdr = &thread.job.captbl;

    let mut root_hdr = root_hdr.write();
    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    let size_bytes = 1 << size_log2;

    let ptr = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(size_bytes))
    }
    .expect("out of memory");

    // SAFETY: This page is valid as the memory has been
    // zeroed (by the invariants of PMAlloc, TODO: check
    // this), and it is of the proper size.
    let _ = slot.replace(unsafe { Page::new(ptr, size_log2) });

    Ok(captr as u64)
}

pub fn sys_pgtbl_create(thread: Arc<Thread>, _intr: InterruptDisabler) -> CapResult<u64> {
    let root_hdr = thread.job.captbl.clone();
    let mut root_hdr = root_hdr.write();
    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    // SAFETY: The page is zeroed and thus is well-defined and valid.
    let pgtbl =
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) };
    let pgtbl = SharedPageTable::from_inner(pgtbl);

    let _ = slot.replace(pgtbl);

    Ok(captr as u64)
}

pub fn sys_endpoint_create(thread: Arc<Thread>, _intr: InterruptDisabler) -> CapResult<u64> {
    let root_hdr = thread.job.captbl.clone();
    let mut root_hdr = root_hdr.write();
    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    let _ = slot.replace(Arc::new(Endpoint {
        recv_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
        send_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
        reply_cap: SpinMutex::new(None),
    }));

    Ok(captr as u64)
}

pub fn sys_page_map(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    from_page: u64,
    into_pgtbl: u64,
    addr: VirtualConst<u8, Identity>,
    flags: PageTableFlags,
) -> CapResult<()> {
    let (from_page, into_pgtbl) = (from_page as usize, into_pgtbl as usize);

    let root_hdr = thread.job.captbl.clone();
    if from_page == into_pgtbl {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = root_hdr.read();
    let mut from_page = root_hdr.get_mut::<capability::paging::Page<DynLevel>>(from_page)?;
    let mut into_pgtbl = root_hdr.get_mut::<capability::paging::PageTable>(into_pgtbl)?;

    into_pgtbl.map_page(&mut from_page, addr, flags)?;

    Ok(())
}

pub fn sys_thread_suspend(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.suspend();

    Ok(())
}

pub fn sys_thread_resume(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.resume()?;

    Ok(())
}

pub fn sys_thread_start(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    entry: u64,
    stack: u64,
    arg1: u64,
    arg2: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;

    let root_hdr = &thread.job.captbl;

    let thread_cap = {
        let root_hdr = root_hdr.read();
        let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;
        thread_cap.cap.thread().unwrap().clone()
    };

    // TODO: make sure this thread hasn't been run to begin with.
    if thread_cap.private.lock().state != ThreadState::Suspended {
        return Err(CapError::InvalidOperation);
    }

    let cap = {
        let root_hdr = root_hdr.read();

        root_hdr
            .get::<AnyCap>(arg1 as usize)
            .ok()
            .map(|x| (x.to_owned_cap(), x.badge, x.rights))
    };

    let captr = if let Some((cap, badge, rights)) = cap {
        let mut captbl = thread_cap.job.captbl.write();
        let captr = captbl.next_slot();
        let mut slot = captbl.get_or_insert_mut(captr).unwrap().replace(cap);
        slot.set_badge(badge);
        slot.update_rights(rights);
        captr
    } else {
        0
    };

    let mut trapframe = thread_cap.trapframe.lock();
    let trapframe = &mut **trapframe;
    trapframe.user_epc = entry;
    trapframe.sp = stack;
    trapframe.a0 = captr as u64;
    trapframe.a1 = arg2;

    let cont = {
        let private = thread_cap.private.lock();
        private.root_pgtbl.is_some()
    };

    if !cont {
        return Err(CapError::InvalidOperation);
    }

    thread_cap.private.lock().state = ThreadState::Runnable;
    Scheduler::enqueue_dl(&thread_cap, None, &mut thread_cap.private.lock());

    Ok(())
}

pub fn sys_thread_configure(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    pgtbl: u64,
    ipc_buffer: u64,
) -> CapResult<()> {
    let (thread_cap, pgtbl, ipc_buffer) =
        (thread_cap as usize, pgtbl as usize, ipc_buffer as usize);

    let root_hdr = thread.job.captbl.clone();
    let root_hdr_lock = root_hdr.read();

    let thread_cap = root_hdr_lock.get::<capability::Thread>(thread_cap)?;

    let pgtbl = if pgtbl == 0 {
        let private = thread.private.lock();
        private.root_pgtbl.as_ref().unwrap().clone()
    } else {
        root_hdr_lock
            .get::<capability::paging::PageTable>(pgtbl)?
            .cap
            .pgtbl()
            .unwrap()
            .clone()
    };

    let ipc_buffer = if ipc_buffer == 0 {
        None
    } else {
        root_hdr_lock
            .get::<capability::paging::Page<DynLevel>>(ipc_buffer)
            .ok()
    };

    // TODO: make sure buffers can't overlap.
    if let Some(ipc_buffer) = ipc_buffer {
        if !ipc_buffer
            .rights
            .contains(CapRights::READ | CapRights::WRITE)
            || ipc_buffer.cap.page().unwrap().size_log2 != BasePage::PAGE_SIZE_LOG2 as u8
        {
            return Err(CapError::InvalidOperation);
        }
        thread_cap.configure(pgtbl, Some(ipc_buffer.cap.page().unwrap().clone()))?;
    } else {
        thread_cap.configure(pgtbl, None)?;
    }

    Ok(())
}

pub fn sys_thread_write_registers(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    regs_vaddr: u64,
) -> CapResult<()> {
    let (thread_cap, regs_vaddr) = (
        thread_cap as usize,
        VirtualConst::<UserRegisters, Identity>::from_usize(regs_vaddr as usize),
    );

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let Some((addr, flags)) = thread
        .private
        .lock()
        .root_pgtbl
        .as_ref()
        .unwrap()
        .walk(regs_vaddr)
    else {
        return Err(CapError::InvalidOperation);
    };
    if !flags.contains(
        crate::paging::PageTableFlags::USER
            | crate::paging::PageTableFlags::READ
            | crate::paging::PageTableFlags::VALID,
    ) {
        return Err(CapError::InvalidOperation);
    }
    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.write_registers(addr.into_virt())?;

    Ok(())
}

// TODO: bring this logic elsewhere so the kernel can signal notifications more cleanly.
pub fn sys_notification_signal(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    notification: u64,
) -> CapResult<()> {
    let notification = notification as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let mut wq = notification.cap.notification().unwrap().wait_queue.lock();
    let word = notification.word();
    word.fetch_or(
        notification.badge().map_or(0, NonZeroU64::get),
        Ordering::Relaxed,
    );
    if let Some(thread) = wq.find_highest_thread() {
        *thread.blocked_on_wq.lock() = None;
        thread.prio_boost();
        drop(wq);
        let hartid = Scheduler::choose_hart(&thread);
        let send_ipi = Scheduler::is_idle(hartid) && hartid != asm::hartid();
        let mut private = thread.private.lock();
        private.state = ThreadState::Runnable;
        //        Scheduler::debug();
        //        crate::println!("===^before^===");
        Scheduler::enqueue_dl(&thread, Some(hartid), &mut private);
        log::trace!("woke up thread={thread:?}, private={private:?}");
        //        Scheduler::debug();
        //        crate::println!("===^after^===\n\n");

        if send_ipi {
            log::trace!("sending IPI to hartid={hartid}");
            //    crate::info!("Sending IPI for thread={thread:#?}");
            //Scheduler::debug();
            sbi::ipi::send_ipi(sbi::HartMask::new(0).with(hartid as usize)).unwrap();
        }
        drop(private);
    }

    Ok(())
}

pub fn sys_notification_poll(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    notification: u64,
) -> CapResult<u64> {
    let notification = notification as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let word = notification.word();
    let res = word.swap(0, Ordering::Relaxed);

    Ok(res)
}

pub fn sys_intr_pool_get(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    pool: u64,
    irq: u64,
) -> CapResult<u64> {
    let (pool, irq) = (pool as usize, irq as u16);

    if irq > 1023 || irq == 0 {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = thread.job.captbl.clone();
    let mut root_hdr = root_hdr.write();

    let mut pool = root_hdr.get_mut::<InterruptPoolCap>(pool)?;
    let handler = pool.make_handler(irq)?;
    drop(pool);

    let captr = root_hdr.next_slot();
    let into_index = root_hdr.get_or_insert_mut(captr)?;

    let _ = into_index.replace(handler);

    Ok(captr as u64)
}

pub fn sys_intr_handler_ack(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    handler: u64,
) -> CapResult<()> {
    let handler = handler as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let handler = root_hdr.get::<InterruptHandlerCap>(handler)?;

    PLIC.hart_sunclaim(handler.cap.intr_handler().unwrap().irq as u32);

    Ok(())
}

pub fn sys_intr_handler_bind(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    handler: u64,
    notification: u64,
) -> CapResult<()> {
    let (handler, notification) = (handler as usize, notification as usize);

    if handler == notification {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let handler = root_hdr.get_mut::<InterruptHandlerCap>(handler)?;
    let notification = root_hdr.get::<NotificationCap>(notification)?;

    handler
        .cap
        .intr_handler()
        .unwrap()
        .notification
        .lock()
        .replace((
            notification.cap.notification().unwrap().clone(),
            notification
                .badge()
                .map(NonZeroU64::get)
                .unwrap_or_default(),
        ));

    Ok(())
}

pub fn sys_intr_handler_unbind(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    handler: u64,
) -> CapResult<()> {
    let handler = handler as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let handler = root_hdr.get::<InterruptHandlerCap>(handler)?;

    *handler.cap.intr_handler().unwrap().notification.lock() = None;

    Ok(())
}

#[allow(clippy::cast_possible_wrap)]
pub fn sys_thread_set_priority(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    prio: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let prio = prio as i64 as i8;

    if !(0..32).contains(&prio) {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    let thread_cap = thread_cap.cap.thread().unwrap();
    let mut private = thread_cap.private.lock();
    Scheduler::dequeue_dl(thread_cap, &mut private);

    let old_eff_prio = private.prio.effective();
    private.prio.base_prio = prio;
    let new_eff_prio = private.prio.effective();
    if new_eff_prio > old_eff_prio {
        Scheduler::enqueue_front_dl(thread_cap, Some(asm::hartid()), &mut private);
    } else {
        Scheduler::enqueue_dl(thread_cap, Some(asm::hartid()), &mut private);
    }
    drop(private);
    if thread_cap
        .blocked_on_wq
        .lock()
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some()
    {
        thread_cap.wait_queue_priority_changed(old_eff_prio, new_eff_prio, true);
    }

    Ok(())
}

pub fn sys_thread_set_ipc_buffer(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    ipc_buffer: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let ipc_buffer = ipc_buffer as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    let thread_cap = thread_cap.cap.thread().unwrap();
    let mut private = thread_cap.private.lock();

    let ipc_buffer = root_hdr.get::<PageCap>(ipc_buffer)?;

    if !ipc_buffer
        .rights
        .contains(CapRights::READ | CapRights::WRITE)
        || ipc_buffer.cap.page().unwrap().size_log2 != BasePage::PAGE_SIZE_LOG2 as u8
    {
        return Err(CapError::InvalidOperation);
    }

    if ipc_buffer.cap.page().unwrap().is_device {
        return Err(CapError::InvalidOperation);
    }

    private.ipc_buffer = Some(ipc_buffer.cap.page().unwrap().clone());

    Ok(())
}

pub fn sys_yield(thread: Arc<Thread>, intr: InterruptDisabler) {
    let mut private = thread.private.lock();
    let old_prio = private.prio.effective();
    Scheduler::dequeue_dl(&thread, &mut private);
    thread.prio_saturating_diminish_dl(&mut private);
    if private.prio.effective() > old_prio {
        Scheduler::enqueue_front_dl(&thread, Some(asm::hartid()), &mut private);
    } else {
        Scheduler::enqueue_dl(&thread, Some(asm::hartid()), &mut private);
    }

    drop(private);
    drop(thread);
    drop(intr);
    // SAFETY: This thread is currently running on this hart.
    unsafe {
        Thread::yield_to_scheduler();
    }
}

#[allow(unused_assignments)]
pub fn sys_notification_wait(
    thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    notification: u64,
) -> CapResult<u64> {
    let notification = notification as usize;

    let root_hdr = thread.job.captbl.clone();
    let root_hdr = root_hdr.read();

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let notification = notification.cap.notification().unwrap();
    let wq = &notification.wait_queue;
    {
        let mut wq_lock = wq.lock();
        let mut private = thread.private.lock();

        let x = notification.word.swap(0, Ordering::Relaxed);
        if x != 0 {
            drop(wq_lock);
            return Ok(x);
        }

        private.state = ThreadState::Blocking;
        Scheduler::dequeue_dl(&thread, &mut private);
        thread.blocked_on_wq.lock().replace(Arc::downgrade(wq));
        wq_lock.insert(private.prio.effective(), &thread);
    }
    drop(thread);
    drop(intr);
    // SAFETY: This thread is currently in the wait queue.
    unsafe {
        Thread::yield_to_scheduler();
    }
    intr = InterruptDisabler::new();

    Ok(notification.word.swap(0, Ordering::Relaxed))
}

pub fn sys_endpoint_recv(
    thread: Arc<Thread>,
    intr: InterruptDisabler,
    endpoint: u64,
    sender_info: u64,
    _dst_slot: u64,
) -> CapResult<MessageHeader> {
    let endpoint = endpoint as usize;

    let sender_info = 'addr: {
        let lock = thread.private.lock();
        let Some(pgtbl) = lock.root_pgtbl.as_ref() else {
            break 'addr ptr::null_mut();
        };
        let Some((addr, flags)) = pgtbl.walk(VirtualConst::from_ptr(sender_info as *const usize))
        else {
            break 'addr ptr::null_mut();
        };
        if flags.contains(
            crate::paging::PageTableFlags::WRITE
                | crate::paging::PageTableFlags::USER
                | crate::paging::PageTableFlags::VALID,
        ) {
            addr.into_virt().into_ptr().cast_mut()
        } else {
            break 'addr ptr::null_mut();
        }
    };

    let endpoint = {
        let root_hdr = thread.job.captbl.clone();
        let root_hdr = root_hdr.read();

        let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;
        endpoint.cap.endpoint().unwrap().clone()
    };
    let (hdr, badge) =
        sys_endpoint_recv_inner(thread, intr, &endpoint).map(|(x, y, _, _)| (x, y))?;

    if !sender_info.is_null() && sender_info.is_aligned() {
        // SAFETY: This address is valid and non-null.
        unsafe { *sender_info = badge as usize };
    }

    Ok(hdr)
}

pub fn sys_endpoint_recv_inner(
    thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: &Arc<Endpoint>,
) -> CapResult<(MessageHeader, u64, Arc<Thread>, InterruptDisabler)> {
    let recv_wq = &endpoint.recv_wait_queue;
    {
        let mut recv_wq_lock = recv_wq.lock();
        let mut private = thread.private.lock();

        let mut send_wq_lock = endpoint.send_wait_queue.lock();
        if let Some(sender) = send_wq_lock.find_highest_thread() {
            let mut sender_private = sender.private.lock();
            let (msg_hdr, badge) = endpoint_recv(
                &thread,
                &sender,
                &mut private,
                &mut sender_private,
                endpoint,
                &mut send_wq_lock,
                false,
            );
            sender_private.recv_fastpath = true;
            drop(private);
            return Ok((msg_hdr, badge, thread, intr));
        }
        drop(send_wq_lock);

        private.state = ThreadState::Blocking;
        Scheduler::dequeue_dl(&thread, &mut private);
        thread.blocked_on_wq.lock().replace(Arc::downgrade(recv_wq));
        recv_wq_lock.insert(private.prio.effective(), &thread);
    }
    drop(thread);
    drop(intr);
    // SAFETY: This thread is in the wait queue
    unsafe {
        Thread::yield_to_scheduler();
    }
    intr = InterruptDisabler::new();

    let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();

    let mut private = thread.private.lock();

    if let Some((hdr, badge)) = private.send_fastpath.take() {
        drop(private);
        return Ok((hdr, badge, thread, intr));
    }

    let mut send_wq_lock = endpoint.send_wait_queue.lock();
    let sender = send_wq_lock.find_highest_thread().unwrap();
    let (msg_hdr, badge) = endpoint_recv(
        &thread,
        &sender,
        &mut private,
        &mut sender.private.lock(),
        endpoint,
        &mut send_wq_lock,
        false,
    );
    drop(private);
    Ok((msg_hdr, badge, thread, intr))
}

#[inline]
fn endpoint_recv(
    thread: &Arc<Thread>,
    sender: &Arc<Thread>,
    private: &mut ThreadProtected,
    sender_private: &mut ThreadProtected,
    endpoint: &Arc<Endpoint>,
    send_wq: &mut WaitQueue,
    send_fastpath: bool,
) -> (MessageHeader, u64) {
    if !send_fastpath {
        endpoint.block_senders(thread, private, send_wq);
        *sender.blocked_on_wq.lock() = None;
    }

    let src_slot = { sender.trapframe.lock().a3 };
    let dest_slot = { thread.trapframe.lock().a3 };

    let sender_root = sender.job.captbl.clone();
    let receiver_root = thread.job.captbl.clone();

    let (hdr, badge) = if send_fastpath {
        private.send_fastpath.unwrap()
    } else {
        // TODO
        let sender_captbl = &sender.job.captbl;
        let sender_captbl = sender_captbl.read();

        let slot = sender_captbl
            .get::<capability::Endpoint>(sender.trapframe.lock().a1 as usize)
            .unwrap();

        (
            MessageHeader::from_raw(sender.trapframe.lock().a2),
            slot.badge.map_or(0, NonZeroU64::get),
        )
    };

    'cap_transfer: {
        if src_slot != 0 && dest_slot != 0 {
            let src_addr = {
                let Some(sender_pgtbl) = sender_private.root_pgtbl.as_ref() else {
                    break 'cap_transfer;
                };
                let Some((src_slot, src_flags)) =
                    sender_pgtbl.walk(VirtualConst::from_ptr(src_slot as *const usize))
                else {
                    break 'cap_transfer;
                };
                if src_flags.contains(
                    crate::paging::PageTableFlags::READ
                        | crate::paging::PageTableFlags::USER
                        | crate::paging::PageTableFlags::VALID,
                ) {
                    src_slot.into_virt().into_ptr()
                } else {
                    break 'cap_transfer;
                }
            };
            let dest_addr = {
                let Some(receiver_pgtbl) = private.root_pgtbl.as_ref() else {
                    break 'cap_transfer;
                };
                let Some((dest_slot, dest_flags)) =
                    receiver_pgtbl.walk(VirtualConst::from_ptr(dest_slot as *const usize))
                else {
                    break 'cap_transfer;
                };
                if dest_flags.contains(
                    crate::paging::PageTableFlags::WRITE
                        | crate::paging::PageTableFlags::USER
                        | crate::paging::PageTableFlags::VALID,
                ) {
                    dest_slot.into_virt().into_ptr()
                } else {
                    break 'cap_transfer;
                }
            };
            if !src_addr.is_null()
                && !dest_addr.is_null()
                && src_addr.is_aligned()
                && dest_addr.is_aligned()
            {
                // SAFETY: This address is valid.
                let src_slot = unsafe { *src_addr };
                // SAFETY: See above.
                let dest_slot = unsafe { &mut *dest_addr.cast_mut() };
                let (cap, badge, rights) = {
                    let sender_root = sender_root.read();
                    let Ok(cap) = sender_root.get_mut::<AnyCap>(src_slot) else {
                        break 'cap_transfer;
                    };
                    (cap.to_owned_cap(), cap.badge, cap.rights)
                };
                let mut receiver_root = receiver_root.write();
                let captr = receiver_root.next_slot();
                if let Ok(slot) = receiver_root.get_or_insert_mut(captr) {
                    let mut slot = slot.replace(cap);
                    // TODO: cap rights here
                    slot.set_badge(badge);
                    slot.update_rights(rights);
                }
                *dest_slot = captr;
            }
        }
    }

    let len = cmp::min(hdr.length(), 4.kib() / mem::size_of::<u64>());
    let tx_trapframe = sender.trapframe.lock();
    let mut rx_trapframe = thread.trapframe.lock();
    match len {
        1 => {
            rx_trapframe.a4 = tx_trapframe.a4;
        }
        2 => {
            rx_trapframe.a4 = tx_trapframe.a4;
            rx_trapframe.a5 = tx_trapframe.a5;
        }
        3 => {
            rx_trapframe.a4 = tx_trapframe.a4;
            rx_trapframe.a5 = tx_trapframe.a5;
            rx_trapframe.a6 = tx_trapframe.a6;
        }
        4 => {
            rx_trapframe.a4 = tx_trapframe.a4;
            rx_trapframe.a5 = tx_trapframe.a5;
            rx_trapframe.a6 = tx_trapframe.a6;
            rx_trapframe.a7 = tx_trapframe.a7;
        }
        _ => {
            if let Some(recv_buffer) = &private.ipc_buffer {
                if let Some(send_buffer) = &sender_private.ipc_buffer {
                    if recv_buffer == send_buffer {
                        // buffers are aliased, no-op (buffers must be
                        // page-aligned).
                    } else {
                        // SAFETY: TODO: deal with page dealloc
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                send_buffer.phys.into_virt().into_ptr(),
                                recv_buffer.phys.into_virt().into_ptr_mut(),
                                len * mem::size_of::<u64>(),
                            );
                        }
                    }
                }
            }
        }
    }

    if !send_fastpath {
        //let hartid = Scheduler::choose_hart_dl(sender, sender_private);
        //let send_ipi = Scheduler::is_idle(hartid) && hartid != asm::hartid();

        sender_private.state = ThreadState::Runnable;
        sender.prio_boost_dl(sender_private);
        Scheduler::enqueue_front_dl(sender, Some(asm::hartid()), sender_private);
        //if send_ipi {
        //    sbi::ipi::send_ipi(sbi::HartMask::new(0).with(hartid as usize)).unwrap();
        //}

        endpoint.unblock_senders(thread, private, send_wq);
    }

    (hdr, badge)
}

// TODO: extract this into a general "wake" function
#[inline]
fn endpoint_send(receiver: &Arc<Thread>, receiver_private: &mut ThreadProtected) {
    *receiver.blocked_on_wq.lock() = None;

    //let hartid = Scheduler::choose_hart_dl(receiver, receiver_private);
    //let send_ipi = Scheduler::is_idle(hartid) && hartid != asm::hartid();

    receiver_private.state = ThreadState::Runnable;
    receiver.prio_boost_dl(receiver_private);
    Scheduler::enqueue_front_dl(receiver, Some(asm::hartid()), receiver_private);
    //if send_ipi {
    //    sbi::ipi::send_ipi(sbi::HartMask::new(0).with(hartid as usize)).unwrap();
    //}
}

#[inline]
pub fn sys_endpoint_send(
    thread: Arc<Thread>,
    intr: InterruptDisabler,
    endpoint: u64,
    hdr: MessageHeader,
    _src_slot: u64,
) -> CapResult<()> {
    let endpoint = endpoint as usize;

    let (endpoint, badge) = {
        let root_hdr = thread.job.captbl.clone();
        let root_hdr = root_hdr.read();

        let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;
        (endpoint.cap.endpoint().unwrap().clone(), endpoint.badge)
    };
    sys_endpoint_send_inner(
        thread,
        intr,
        &endpoint,
        hdr,
        badge.map_or(0, NonZeroU64::get),
    )
    .map(|_| ())
}

#[inline]
pub fn sys_endpoint_send_inner(
    thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: &Arc<Endpoint>,
    hdr: MessageHeader,
    badge: u64,
) -> CapResult<(Arc<Thread>, InterruptDisabler)> {
    let send_wq = &endpoint.send_wait_queue;
    {
        let mut send_wq_lock = send_wq.lock();
        let mut private = thread.private.lock();

        let mut recv_wq_lock = endpoint.recv_wait_queue.lock();
        if let Some(receiver) = recv_wq_lock.find_highest_thread() {
            let mut receiver_private = receiver.private.lock();
            receiver_private.send_fastpath = Some((hdr, badge));
            drop(recv_wq_lock);
            endpoint_recv(
                &receiver,
                &thread,
                &mut receiver_private,
                &mut private,
                endpoint,
                &mut send_wq_lock,
                true,
            );
            endpoint_send(&receiver, &mut receiver_private);
            drop(private);
            return Ok((thread, intr));
        }
        drop(recv_wq_lock);

        private.state = ThreadState::Blocking;
        Scheduler::dequeue_dl(&thread, &mut private);
        thread.blocked_on_wq.lock().replace(Arc::downgrade(send_wq));
        send_wq_lock.insert(private.prio.effective(), &thread);
    }
    drop(thread);
    drop(intr);
    // SAFETY: This thread is in the wait queue
    unsafe {
        Thread::yield_to_scheduler();
    }
    intr = InterruptDisabler::new();

    let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();

    let mut private = thread.private.lock();

    if private.recv_fastpath {
        private.recv_fastpath = false;
        drop(private);
        return Ok((thread, intr));
    }

    let receiver = endpoint
        .recv_wait_queue
        .lock()
        .find_highest_thread()
        .unwrap();
    endpoint_send(&receiver, &mut receiver.private.lock());

    drop(private);
    Ok((thread, intr))
}

pub fn sys_endpoint_reply(
    thread: Arc<Thread>,
    intr: InterruptDisabler,
    base_endpoint: u64,
    hdr: MessageHeader,
    _src_slot: u64,
) -> CapResult<()> {
    let base_endpoint = base_endpoint as usize;

    let (endpoint, badge) = {
        let root_hdr = thread.job.captbl.clone();
        let root_hdr = root_hdr.read();

        let base_endpoint_slot = root_hdr.get::<capability::Endpoint>(base_endpoint)?;
        let base_endpoint = base_endpoint_slot.cap.endpoint().unwrap();
        let lock = base_endpoint.reply_cap.lock();
        (
            lock.clone().ok_or(CapError::InvalidOperation)?,
            base_endpoint_slot.badge,
        )
    };
    sys_endpoint_send_inner(
        thread,
        intr,
        &endpoint,
        hdr,
        badge.map_or(0, NonZeroU64::get),
    )
    .map(|_| ())
}

// TODO: reply_recv a1 = endpt, a2 = hdr, a3 = &badge, a4_in=cap a4_lateout-a7 = MRs

pub fn sys_endpoint_call(
    mut thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: u64,
    hdr: MessageHeader,
    _cap_slot: u64,
) -> CapResult<MessageHeader> {
    let endpoint = endpoint as usize;

    let (endpoint, badge) = {
        let root_hdr = thread.job.captbl.clone();
        let root_hdr = root_hdr.read();

        let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;

        (endpoint.cap.endpoint().unwrap().clone(), endpoint.badge)
    };

    {
        let reply_cap = Arc::new(Endpoint {
            recv_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
            send_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
            reply_cap: SpinMutex::new(None),
        });
        endpoint.reply_cap.lock().replace(reply_cap);

        (thread, intr) = sys_endpoint_send_inner(
            thread,
            intr,
            &endpoint,
            hdr,
            badge.map_or(0, NonZeroU64::get),
        )?;
    }

    let hdr = {
        let reply_cap = endpoint.reply_cap.lock().clone().unwrap();
        let (hdr, _, _, _) = sys_endpoint_recv_inner(thread, intr, &reply_cap)?;
        *endpoint.reply_cap.lock() = None;
        hdr
    };

    Ok(hdr)
}
// TODO: somehow prevent the system from blocking completely if the
// thread being sent to doesn't have an ipc buffer

pub fn sys_job_create_thread(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    job: u64,
    name_ptr: VirtualConst<u8, Identity>,
    name_len: u64,
) -> CapResult<u64> {
    let job = job as usize;
    let name_len = name_len as usize;

    let root_hdr = thread.job.captbl.clone();

    let private = thread.private.lock();

    let root_hdr_lock = root_hdr.read();
    let job = root_hdr_lock.get::<JobCap>(job)?.cap.job().unwrap().clone();

    // TODO: revive copy_from_user.
    if name_len >= 4usize.kib() - name_ptr.page_offset().into_usize() {
        return Err(CapError::InvalidOperation);
    }

    let name = if name_ptr == VirtualConst::null() {
        String::new()
    } else {
        let mut name = String::with_capacity(name_len);
        let name_addr = 'addr: {
            let Some(pgtbl) = private.root_pgtbl.as_ref() else {
                break 'addr ptr::null();
            };
            let Some((addr, flags)) = pgtbl.walk(name_ptr) else {
                break 'addr ptr::null();
            };
            if flags.contains(
                crate::paging::PageTableFlags::READ
                    | crate::paging::PageTableFlags::USER
                    | crate::paging::PageTableFlags::VALID,
            ) {
                addr.into_virt().into_ptr()
            } else {
                break 'addr ptr::null();
            }
        };

        if !name_addr.is_null() {
            // SAFETY: This memory must be valid for up to the length we determined.
            let slice = core::str::from_utf8(unsafe { slice::from_raw_parts(name_addr, name_len) })
                .map_err(|_| CapError::InvalidOperation)?;

            // SAFETY: This memory is valid and initialized. We will validate UTF-8
            name.push_str(slice);
        }

        name
    };

    drop(root_hdr_lock);
    let mut root_hdr = root_hdr.write();

    let captr = root_hdr.next_slot();

    let slot = root_hdr.get_or_insert_mut(captr)?;

    let _ = slot.replace(Thread::new(name, None, job));

    Ok(captr as u64)
}
