#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments
)]
use core::{
    cmp,
    mem::{self, MaybeUninit},
    num::NonZeroU64,
    slice,
    sync::atomic::AtomicU64,
};

use alloc::{
    boxed::Box,
    string::String,
    sync::{Arc, Weak},
};
use atomic::Ordering;
use rille::{
    addr::{Identity, Kernel, VirtualConst},
    capability::{
        self,
        paging::{BasePage, DynLevel, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel},
        Allocator, AnyCap, CapError, CapResult, CapRights, CapabilityType, Empty, MessageHeader,
        UserRegisters,
    },
    units::StorageUnits,
};

use crate::{
    asm::{self, InterruptDisabler},
    capability::{
        captbl::Captbl, slotref::SlotRefMut, CapToOwned, Endpoint, InterruptHandlerCap,
        InterruptPoolCap, Notification, NotificationCap, Page, PageCap, Thread, ThreadProtected,
        ThreadState, WaitQueue,
    },
    hart_local::LOCAL_HART,
    kalloc::{self, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    plic::PLIC,
    print, println,
    sched::Scheduler,
    symbol,
    sync::{SpinMutex, SpinRwLock},
};

pub fn sys_debug_dump_root(_thread: Arc<Thread>, _intr: InterruptDisabler) -> Result<(), CapError> {
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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

pub fn sys_debug_cap_slot(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    tbl: u64,
    index: u64,
) -> Result<(), CapError> {
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

pub fn sys_debug_print(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    str_ptr: VirtualConst<u8, Identity>,
    str_len: u64,
) {
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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

pub fn sys_delete(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    table: u64,
    index: u64,
) -> CapResult<()> {
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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
                perform_notification_alloc(slot)?;
            }
        }

        CapabilityType::Endpoint => {
            for i in 0..count {
                let slot = into_index.get_mut::<Empty>(starting_at + i)?;
                perform_endpoint_alloc(slot)?;
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

fn perform_notification_alloc(slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    let _ = slot.replace(Arc::new(Notification {
        word: AtomicU64::new(0),
        wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
    }));

    Ok(())
}

fn perform_endpoint_alloc(slot: SlotRefMut<'_, Empty>) -> CapResult<()> {
    let _ = slot.replace(Arc::new(Endpoint {
        recv_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
        send_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
        reply_cap: SpinMutex::new(None),
    }));

    Ok(())
}

pub fn sys_page_map(
    thread: Arc<Thread>,
    intr: InterruptDisabler,
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

pub fn sys_thread_suspend(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };
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
    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.resume()?;

    Ok(())
}

pub fn sys_thread_configure(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    thread_cap: u64,
    captbl: u64,
    pgtbl: u64,
    ipc_buffer: u64,
) -> CapResult<()> {
    let (thread_cap, captbl, pgtbl, ipc_buffer) = (
        thread_cap as usize,
        captbl as usize,
        pgtbl as usize,
        ipc_buffer as usize,
    );

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

    let ipc_buffer = if ipc_buffer == 0 {
        None
    } else {
        root_hdr
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
        thread_cap.configure(captbl, pgtbl, Some(ipc_buffer.cap.page().unwrap().phys))?;
    } else {
        thread_cap.configure(captbl, pgtbl, None)?;
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

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<capability::Thread>(thread_cap)?;

    thread_cap.write_registers(regs_vaddr)?;

    Ok(())
}

// TODO: bring this logic elsewhere so the kernel can signal notifications more cleanly.
pub fn sys_notification_signal(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    notification: u64,
) -> CapResult<()> {
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

pub fn sys_notification_poll(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    notification: u64,
) -> CapResult<u64> {
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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

pub fn sys_intr_handler_ack(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    handler: u64,
) -> CapResult<()> {
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
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
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

pub fn sys_intr_handler_unbind(
    thread: Arc<Thread>,
    _intr: InterruptDisabler,
    handler: u64,
) -> CapResult<()> {
    let handler = handler as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

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

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

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

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

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

    private.ipc_buffer = Some(ipc_buffer.cap.page().unwrap().phys);

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

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let notification = root_hdr.get::<NotificationCap>(notification)?;

    let notification = notification.cap.notification().unwrap();
    let wq = &notification.wait_queue;
    {
        let mut wq_lock = wq.lock();
        let mut private = thread.private.lock();

        let x = notification.word.swap(0, Ordering::Relaxed);
        if x != 0 {
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
) -> CapResult<MessageHeader> {
    let endpoint = endpoint as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;
    let endpoint = endpoint.cap.endpoint().unwrap();

    sys_endpoint_recv_inner(thread, intr, endpoint).map(|(x, _, _)| x)
}

pub fn sys_endpoint_recv_inner(
    thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: &Arc<Endpoint>,
) -> CapResult<(MessageHeader, Arc<Thread>, InterruptDisabler)> {
    let recv_wq = &endpoint.recv_wait_queue;
    {
        let mut recv_wq_lock = recv_wq.lock();
        let mut private = thread.private.lock();

        let mut send_wq_lock = endpoint.send_wait_queue.lock();
        if let Some(sender) = send_wq_lock.find_highest_thread() {
            let mut sender_private = sender.private.lock();
            let msg_hdr = endpoint_recv(
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
            return Ok((msg_hdr, thread, intr));
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

    if let Some(hdr) = private.send_fastpath.take() {
        drop(private);
        return Ok((hdr, thread, intr));
    }

    let mut send_wq_lock = endpoint.send_wait_queue.lock();
    let sender = send_wq_lock.find_highest_thread().unwrap();
    let msg_hdr = endpoint_recv(
        &thread,
        &sender,
        &mut private,
        &mut sender.private.lock(),
        endpoint,
        &mut send_wq_lock,
        false,
    );
    drop(private);
    Ok((msg_hdr, thread, intr))
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
) -> MessageHeader {
    if !send_fastpath {
        endpoint.block_senders(thread, private, send_wq);
        *sender.blocked_on_wq.lock() = None;
    }

    let hdr = if send_fastpath {
        private.send_fastpath.unwrap()
    } else {
        MessageHeader::from_raw(sender.trapframe.lock().a2)
    };

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
            if let Some(recv_buffer) = private.ipc_buffer {
                if let Some(send_buffer) = sender_private.ipc_buffer {
                    // SAFETY: TODO: deal with page dealloc
                    unsafe {
                        core::ptr::copy(
                            send_buffer.into_virt().into_ptr(),
                            recv_buffer.into_virt().into_ptr_mut(),
                            len * mem::size_of::<u64>(),
                        );
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

    hdr
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
) -> CapResult<()> {
    let endpoint = endpoint as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;
    let endpoint = endpoint.cap.endpoint().unwrap();
    sys_endpoint_send_inner(thread, intr, endpoint, hdr).map(|_| ())
}

#[inline]
pub fn sys_endpoint_send_inner(
    thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: &Arc<Endpoint>,
    hdr: MessageHeader,
) -> CapResult<(Arc<Thread>, InterruptDisabler)> {
    let send_wq = &endpoint.send_wait_queue;
    {
        let mut send_wq_lock = send_wq.lock();
        let mut private = thread.private.lock();

        let mut recv_wq_lock = endpoint.recv_wait_queue.lock();
        if let Some(receiver) = recv_wq_lock.find_highest_thread() {
            let mut receiver_private = receiver.private.lock();
            receiver_private.send_fastpath = Some(hdr);
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
) -> CapResult<()> {
    let base_endpoint = base_endpoint as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let base_endpoint = root_hdr.get::<capability::Endpoint>(base_endpoint)?;
    let base_endpoint = base_endpoint.cap.endpoint().unwrap();
    let endpoint = base_endpoint
        .reply_cap
        .lock()
        .clone()
        .ok_or(CapError::InvalidOperation)?;
    sys_endpoint_send_inner(thread, intr, &endpoint, hdr).map(|_| ())
}

// TODO: reply_recv a1 = endpt, a2 = hdr, a3 = &badge, a4-a7 = MRs

pub fn sys_endpoint_call(
    mut thread: Arc<Thread>,
    mut intr: InterruptDisabler,
    endpoint: u64,
    hdr: MessageHeader,
) -> CapResult<MessageHeader> {
    let endpoint = endpoint as usize;

    let root_hdr = {
        let private = thread.private.lock();
        private.captbl.as_ref().unwrap().clone()
    };

    let endpoint = root_hdr.get::<capability::Endpoint>(endpoint)?;

    let endpoint = endpoint.cap.endpoint().unwrap();

    {
        let reply_cap = Arc::new(Endpoint {
            recv_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
            send_wait_queue: Arc::new(SpinMutex::new(WaitQueue::new(None))),
            reply_cap: SpinMutex::new(None),
        });
        endpoint.reply_cap.lock().replace(reply_cap);

        (thread, intr) = sys_endpoint_send_inner(thread, intr, endpoint, hdr)?;
    }

    let hdr = {
        let reply_cap = endpoint.reply_cap.lock().clone().unwrap();
        let (hdr, _, _) = sys_endpoint_recv_inner(thread, intr, &reply_cap)?;
        *endpoint.reply_cap.lock() = None;
        hdr
    };

    Ok(hdr)
}
