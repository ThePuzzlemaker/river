#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments
)]
use core::{mem::MaybeUninit, num::NonZeroU64, slice, sync::atomic::AtomicU64};

use alloc::{boxed::Box, string::String, sync::Arc};
use atomic::Ordering;
use rille::{
    addr::{Identity, Kernel, VirtualConst},
    capability::{
        self,
        paging::{BasePage, DynLevel, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel},
        Allocator, AnyCap, CapError, CapResult, CapRights, CapabilityType, Empty, UserRegisters,
    },
    units::StorageUnits,
};

use crate::{
    asm,
    capability::{
        captbl::Captbl, slotref::SlotRefMut, CapToOwned, InterruptHandlerCap, InterruptPoolCap,
        Notification, NotificationCap, OwnedWaitQueue, Page, Thread, ThreadState, WaitQueue,
    },
    kalloc::{self, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    plic::PLIC,
    print, println,
    sched::Scheduler,
    symbol,
    sync::{SpinMutex, SpinRwLock},
};

pub fn sys_debug_dump_root(_thread: &Arc<Thread>) -> Result<(), CapError> {
    //crate::println!("{:#x?}", _thread.private.lock().captbl);
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
    crate::info!("thread.prio={:#?}", &_thread.private.lock().prio);
    Ok(())
}

pub fn sys_debug_cap_identify(
    thread: &Arc<Thread>,
    tbl: u64,
    index: u64,
) -> Result<CapabilityType, CapError> {
    let (tbl, index) = (tbl as usize, index as usize);

    let root_hdr = &thread
        .private
        .lock()
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(tbl)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let slot = tbl.get::<AnyCap>(index)?;
    Ok(slot.cap.cap_type())
}

pub fn sys_debug_cap_slot(thread: &Arc<Thread>, tbl: u64, index: u64) -> Result<(), CapError> {
    let (tbl, index) = (tbl as usize, index as usize);

    let root_hdr = &thread
        .private
        .lock()
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)
        .map(|x| {
            println!("present 1");
            x
        })?;

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(tbl)
            .map(|x| {
                println!("present 2");
                x
            })?
            .to_owned_cap()
            .upgrade()
            .map(|x| {
                println!("present 3");
                x
            })
            .ok_or(CapError::NotPresent)?
    };

    let slot = tbl.get::<AnyCap>(index).map(|x| {
        println!("present 4");
        x
    })?;
    println!("{:#x?}", slot);

    Ok(())
}

pub fn sys_debug_print(thread: &Arc<Thread>, str_ptr: VirtualConst<u8, Identity>, str_len: u64) {
    let str_len = str_len as usize;

    let mut private = thread.private.lock();

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
                .as_mut()
                .unwrap()
                .walk(str_ptr)
                .unwrap()
                .0
                .into_virt()
                .into_ptr(),
            str_len,
        )
    };

    let s = core::str::from_utf8(slice).unwrap();
    print!("{}", s);
}

pub fn sys_copy_deep(
    thread: &Arc<Thread>,
    from_tbl_ref: u64,
    from_tbl_index: u64,
    from_index: u64,
    into_tbl_ref: u64,
    into_tbl_index: u64,
    into_index: u64,
) -> Result<(), CapError> {
    let from_tbl_ref = from_tbl_ref as usize;
    let from_tbl_index = from_tbl_index as usize;
    let from_index = from_index as usize;
    let into_tbl_ref = into_tbl_ref as usize;
    let into_tbl_index = into_tbl_index as usize;
    let into_index = into_index as usize;

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let from_tbl_ref = if from_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(from_tbl_ref)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let from_tbl_index = from_tbl_ref
        .get::<capability::Captbl>(from_tbl_index)?
        .to_owned_cap()
        .upgrade()
        .ok_or(CapError::NotPresent)?;
    drop(from_tbl_ref);

    let into_tbl_ref = if into_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(into_tbl_ref)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let into_tbl_index = into_tbl_ref
        .get::<capability::Captbl>(into_tbl_index)?
        .to_owned_cap()
        .upgrade()
        .ok_or(CapError::NotPresent)?;
    drop(into_tbl_ref);

    if from_tbl_index == into_tbl_index && from_index == into_index {
        // Captrs are aliased, this is a no-op.
        return Ok(());
    }

    let into_index = into_tbl_index.get_mut::<Empty>(into_index)?;

    let from_index = from_tbl_index.get_mut::<AnyCap>(from_index)?;

    // We don't need to call copy_hook here since replace handles that
    // for us.
    let mut _into_index = into_index.replace(from_index.to_owned_cap());

    Ok(())
}

pub fn sys_grant(
    thread: &Arc<Thread>,
    from_table: u64,
    from_index: u64,
    into_table: u64,
    into_index: u64,
    rights: CapRights,
    badge: u64,
) -> CapResult<()> {
    let (from_table, from_index, into_table, into_index) = (
        from_table as usize,
        from_index as usize,
        into_table as usize,
        into_index as usize,
    );
    let (from_table_, from_index_, into_table_, into_index_) =
        (from_table, from_index, into_table, into_index);

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let from_table = if from_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(from_table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let into_table = if into_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(into_table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    if from_table_ == into_table_ && from_index_ == into_index_ {
        let mut into = into_table.get_mut::<AnyCap>(into_index)?;
        // Captrs are aliased, we just need to mutate.
        into.set_badge(NonZeroU64::new(badge));
        into.update_rights(rights);
        return Ok(());
    }

    let from = from_table.get_mut::<AnyCap>(from_index)?;
    let into = into_table.get_mut::<Empty>(into_index)?;

    // We don't need to call copy_hook here since replace handles that
    // for us.
    let mut into = into.replace(from.to_owned_cap());

    // TODO: badge validation
    into.set_badge(NonZeroU64::new(badge));
    into.update_rights(rights);

    Ok(())
}

pub fn sys_swap(
    thread: &Arc<Thread>,
    cap1_table: u64,
    cap1_index: u64,
    cap2_table: u64,
    cap2_index: u64,
) -> Result<(), CapError> {
    let (cap1_table, cap1_index, cap2_table, cap2_index) = (
        cap1_table as usize,
        cap1_index as usize,
        cap2_table as usize,
        cap2_index as usize,
    );

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    if cap1_table == cap2_table && cap1_index == cap2_index {
        // Captrs are aliased, this is a no-op
        return Ok(());
    }

    let cap1_table = if cap1_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(cap1_table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let cap2_table = if cap2_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(cap2_table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let cap1 = cap1_table.get_mut::<AnyCap>(cap1_index)?;
    let cap2 = cap2_table.get_mut::<AnyCap>(cap2_index)?;

    cap1.swap(cap2);

    Ok(())
}

pub fn sys_delete(thread: &Arc<Thread>, table: u64, index: u64) -> CapResult<()> {
    let (table, index) = (table as usize, index as usize);

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let table = if table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };
    let slot = table.get_mut::<AnyCap>(index)?;

    let _ = slot.delete();

    Ok(())
}

pub fn sys_allocate_many(
    thread: &Arc<Thread>,
    allocator: u64,
    into_ref: u64,
    into_index: u64,
    starting_at: u64,
    count: u64,
    cap_type: CapabilityType,
    size: u64,
) -> Result<(), CapError> {
    let (allocator, into_ref, into_index, starting_at, count) = (
        allocator as usize,
        into_ref as usize,
        into_index as usize,
        starting_at as usize,
        count as usize,
    );
    let size = size as u8;

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    // Check that we actually have an allocator cap, before we do more
    // processing.
    let _alloc = root_hdr.get::<Allocator>(allocator)?;

    let into_ref = if into_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(into_ref)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let into_index = into_ref
        .get::<capability::Captbl>(into_index)?
        .to_owned_cap()
        .upgrade()
        .ok_or(CapError::NotPresent)?;

    if &into_index == root_hdr && (starting_at..starting_at + count).contains(&allocator) {
        // Captrs are aliased, this is invalid.
        return Err(CapError::InvalidOperation);
    }

    match cap_type {
        CapabilityType::Empty => {}
        CapabilityType::Captbl => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_captbl_alloc(size, slot)?;
            }
        }
        CapabilityType::Page => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_page_alloc(size, slot)?;
            }
        }
        CapabilityType::PgTbl => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_pgtbl_alloc(slot)?;
            }
        }
        CapabilityType::Thread => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_thread_alloc(slot)?;
            }
        }
        CapabilityType::Notification => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_notification_alloc(thread, slot)?;
            }
        }

        CapabilityType::Notification => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_notification_alloc(thread, slot)?;
            }
        }

        _ => return Err(CapError::InvalidOperation),
    }

    Ok(())
}

fn perform_captbl_alloc(n_slots_log2: u8, slot: SlotRefMut<'_, Empty>) -> Result<(), CapError> {
    let size_bytes_log2 = n_slots_log2 + 6;
    if !(12..=64).contains(&size_bytes_log2) {
        return Err(CapError::InvalidSize);
    }
    let size_bytes = 1 << size_bytes_log2;

    let ptr = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(size_bytes))
    };

    if ptr.is_none() {
        return Err(CapError::NotEnoughResources);
    }

    let ptr = ptr.unwrap();

    // SAFETY: This captbl is valid as the memory has been zeroed (by
    // the invariants of PMAlloc), and is of the proper size.
    let _ = slot.replace(unsafe { Captbl::new(ptr.into_virt(), n_slots_log2) }.downgrade());

    Ok(())
}

fn perform_page_alloc(size_log2: u8, slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    if ![
        BasePage::PAGE_SIZE_LOG2 as u8,
        MegaPage::PAGE_SIZE_LOG2 as u8,
        GigaPage::PAGE_SIZE_LOG2 as u8,
    ]
    .contains(&size_log2)
    {
        return Err(CapError::InvalidSize);
    }

    let size_bytes = 1 << size_log2;

    let ptr = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(size_bytes))
    };

    if ptr.is_none() {
        return Err(CapError::NotEnoughResources);
    }

    let ptr = ptr.unwrap();

    // SAFETY: The page is valid as the memory has been zeroed (by the
    // invariants of PMAlloc) and it is of the proper size.
    let _ = slot.replace(unsafe { Page::new(ptr, size_log2) });

    Ok(())
}

fn perform_pgtbl_alloc(slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    use crate::paging::PageTableFlags as KernelFlags;

    // SAFETY: The page is zeroed and thus is well-defined and valid.
    let pgtbl =
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) };
    let pgtbl = Arc::new(SpinRwLock::new(pgtbl));
    let pgtbl = SharedPageTable::from_inner(pgtbl);

    let trampoline_virt =
        VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());

    pgtbl.map(
        trampoline_virt.into_phys().into_identity(),
        VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
        KernelFlags::VAD | KernelFlags::RX,
        PageSize::Base,
    );

    let _ = slot.replace(pgtbl);

    Ok(())
}

fn perform_thread_alloc(slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    let thread = Thread::new(String::new(), None, None);

    let _ = slot.replace(thread);

    Ok(())
}

fn perform_notification_alloc(thread: &Arc<Thread>, slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    let _ = slot.replace(Arc::new(Notification {
        word: AtomicU64::new(0),
        wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
    }));

    Ok(())
}

pub fn sys_page_map(
    thread: &Arc<Thread>,
    from_page: u64,
    into_pgtbl: u64,
    addr: VirtualConst<u8, Identity>,
    flags: PageTableFlags,
) -> CapResult<()> {
    let (from_page, into_pgtbl) = (from_page as usize, into_pgtbl as usize);

    let private = thread.private.lock();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    if from_page == into_pgtbl {
        return Err(CapError::InvalidOperation);
    }

    let mut from_page = root_hdr.get_mut::<capability::paging::Page<DynLevel>>(from_page)?;
    let mut into_pgtbl = root_hdr.get_mut::<capability::paging::PageTable>(into_pgtbl)?;

    into_pgtbl.map_page(&mut from_page, addr, flags)?;

    Ok(())
}

pub fn sys_thread_suspend(thread: &Arc<Thread>, thread_cap: u64) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };
    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.suspend();

    Ok(())
}

pub fn sys_thread_resume(thread: &Arc<Thread>, thread_cap: u64) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.resume()?;

    Ok(())
}

pub fn sys_thread_configure(
    thread: &Arc<Thread>,
    thread_cap: u64,
    captbl: u64,
    pgtbl: u64,
) -> CapResult<()> {
    let (thread_cap, captbl, pgtbl) = (thread_cap as usize, captbl as usize, pgtbl as usize);

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;
    let captbl = if captbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(captbl)?
            .cap
            .captbl()
            .unwrap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let pgtbl = if pgtbl == 0 {
        let private = thread.private.lock();
        private.root_pgtbl.as_ref().unwrap().clone()
    } else {
        root_hdr
            .get::<capability::paging::PageTable>(pgtbl)?
            .cap
            .pgtbl()
            .unwrap()
            .clone()
    };

    thread_cap.configure(captbl, pgtbl)?;

    Ok(())
}

pub fn sys_thread_write_registers(
    thread: &Arc<Thread>,
    thread_cap: u64,
    regs_vaddr: u64,
) -> CapResult<()> {
    let (thread_cap, regs_vaddr) = (
        thread_cap as usize,
        VirtualConst::<UserRegisters, Identity>::from_usize(regs_vaddr as usize),
    );

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.write_registers(regs_vaddr)?;

    Ok(())
}

// TODO: bring this logic elsewhere so the kernel can signal notifications more cleanly.
pub fn sys_notification_signal(thread: &Arc<Thread>, notification: u64) -> CapResult<()> {
    let notification = notification as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let mut wq = notification.cap.notification().unwrap().wait_queue.lock();
    let word = notification.word();
    word.fetch_or(
        notification.badge().map_or(0, NonZeroU64::get),
        Ordering::Relaxed,
    );
    //    log::trace!("signal word={word:x?}");
    log::trace!("wq={wq:?}");
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

pub fn sys_notification_poll(thread: &Arc<Thread>, notification: u64) -> CapResult<u64> {
    let notification = notification as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let word = notification.word();
    let res = word.swap(0, Ordering::Relaxed);

    Ok(res)
}

pub fn sys_intr_pool_get(
    thread: &Arc<Thread>,
    pool: u64,
    into_table: u64,
    into_index: u64,
    irq: u64,
) -> CapResult<()> {
    let (pool, into_table, into_index, irq) = (
        pool as usize,
        into_table as usize,
        into_index as usize,
        irq as u16,
    );

    if irq > 1023 || irq == 0 {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    if into_table == 0 && pool == into_index {
        // Captrs are aliased, this is invalid.
        return Err(CapError::InvalidOperation);
    }

    let into_table = if into_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<capability::Captbl>(into_table)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let mut pool = root_hdr.get_mut::<InterruptPoolCap>(pool)?;
    let into_index = into_table.get_mut::<capability::Empty>(into_index)?;

    let handler = pool.make_handler(irq)?;
    let _ = into_index.replace(handler);

    Ok(())
}

pub fn sys_intr_handler_ack(thread: &Arc<Thread>, handler: u64) -> CapResult<()> {
    let handler = handler as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let handler = root_hdr.get::<InterruptHandlerCap>(handler)?;

    PLIC.hart_sunclaim(handler.cap.intr_handler().unwrap().irq as u32);

    Ok(())
}

pub fn sys_intr_handler_bind(
    thread: &Arc<Thread>,
    handler: u64,
    notification: u64,
) -> CapResult<()> {
    let (handler, notification) = (handler as usize, notification as usize);

    if handler == notification {
        return Err(CapError::InvalidOperation);
    }

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

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

pub fn sys_intr_handler_unbind(thread: &Arc<Thread>, handler: u64) -> CapResult<()> {
    let handler = handler as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let handler = root_hdr.get::<InterruptHandlerCap>(handler)?;

    *handler.cap.intr_handler().unwrap().notification.lock() = None;

    Ok(())
}
