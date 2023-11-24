#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments
)]
use core::{mem::MaybeUninit, slice};

use alloc::{boxed::Box, string::String, sync::Arc};
use rille::{
    addr::{Identity, Kernel, VirtualConst},
    capability::{
        paging::{BasePage, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel},
        CapError, CapResult, CapabilityType, UserRegisters,
    },
    units::StorageUnits,
};

use crate::{
    capability::{
        captbl::Captbl, slotref::SlotRefMut, AllocatorSlot, AnyCap, CapToOwned, EmptySlot, Page,
        PgTbl, Thread,
    },
    kalloc::{self, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    print, println, symbol,
    sync::SpinRwLock,
};

pub fn sys_debug_dump_root(thread: &Arc<Thread>) -> Result<(), CapError> {
    crate::println!("{:#x?}", thread.private.read().captbl);
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
        .read()
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<Captbl>(tbl)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let slot = tbl.get::<AnyCap>(index)?;
    Ok(slot.cap_type())
}

pub fn sys_debug_cap_slot(thread: &Arc<Thread>, tbl: u64, index: u64) -> Result<(), CapError> {
    let (tbl, index) = (tbl as usize, index as usize);

    let root_hdr = &thread
        .private
        .read()
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<Captbl>(tbl)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let slot = tbl.get::<AnyCap>(index)?;
    println!("{:#x?}", slot);

    Ok(())
}

pub fn sys_debug_print(thread: &Arc<Thread>, str_ptr: VirtualConst<u8, Identity>, str_len: u64) {
    let str_len = str_len as usize;

    let mut private = thread.private.write();

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

    let private = thread.private.read();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let from_tbl_ref = if from_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<Captbl>(from_tbl_ref)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let from_tbl_index = from_tbl_ref
        .get::<Captbl>(from_tbl_index)?
        .to_owned_cap()
        .upgrade()
        .ok_or(CapError::NotPresent)?;
    drop(from_tbl_ref);

    let into_tbl_ref = if into_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<Captbl>(into_tbl_ref)?
            .to_owned_cap()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let into_tbl_index = into_tbl_ref
        .get::<Captbl>(into_tbl_index)?
        .to_owned_cap()
        .upgrade()
        .ok_or(CapError::NotPresent)?;
    drop(into_tbl_ref);

    if from_tbl_index == into_tbl_index && from_index == into_index {
        // Captrs are aliased, this is a no-op.
        return Ok(());
    }

    let into_index = into_tbl_index.get_mut::<EmptySlot>(into_index)?;

    // if let Ok(mut cap) = from_tbl_index.get_mut::<Untyped>(from_index) {
    //     // We can't retype if we are untyped and have a child or
    //     // allocation.
    //     if cap.free_addr() > cap.base_addr() || cap.has_child() {
    //         return Err(CapError::RevokeFirst);
    //     }

    //     // Copying an Untyped acts like a retype.
    //     cap.set_free_addr(cap.base_addr().add(1 << cap.size_log2()));
    // }

    let mut from_index = from_tbl_index.get_mut::<AnyCap>(from_index)?;

    let mut into_index = into_index.replace(from_index.to_owned_cap());
    from_index.add_child(&mut into_index);

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

    let private = thread.private.read();

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
            .get(cap1_table)?
            .clone()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let cap2_table = if cap2_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get(cap2_table)?
            .clone()
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let cap1 = cap1_table.get_mut::<AnyCap>(cap1_index)?;
    let cap2 = cap2_table.get_mut::<AnyCap>(cap2_index)?;

    cap1.swap(cap2)?;

    Ok(())
}

pub fn sys_delete(thread: &Arc<Thread>, table: u64, index: u64) -> CapResult<()> {
    let (table, index) = (table as usize, index as usize);

    let private = thread.private.read();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    let table = if table == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get(table)?
            .clone()
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

    let private = thread.private.read();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    // Check that we actually have an allocator cap, before we do more
    // processing.
    let _alloc = root_hdr.get::<AllocatorSlot>(allocator)?;

    let into_ref = if into_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get(into_ref)?
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let into_index = into_ref
        .get::<Captbl>(into_index)?
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
                let slot = into_index.get_mut::<EmptySlot>(starting_at + i)?;
                perform_captbl_alloc(size, slot)?;
            }
        }
        CapabilityType::Page => {
            for i in 0..count {
                let slot = into_index.get_mut::<EmptySlot>(starting_at + i)?;
                perform_page_alloc(size, slot)?;
            }
        }
        CapabilityType::PgTbl => {
            for i in 0..count {
                let slot = into_index.get_mut::<EmptySlot>(starting_at + i)?;
                perform_pgtbl_alloc(slot)?;
            }
        }
        CapabilityType::Thread => {
            for i in 0..count {
                let slot = into_index.get_mut::<EmptySlot>(starting_at + i)?;
                perform_thread_alloc(slot)?;
            }
        }

        _ => return Err(CapError::InvalidOperation),
    }

    Ok(())
}

fn perform_captbl_alloc(n_slots_log2: u8, slot: SlotRefMut<'_, EmptySlot>) -> Result<(), CapError> {
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
    let _ = slot.replace(unsafe { Captbl::new(ptr.into_virt(), n_slots_log2) });

    Ok(())
}

fn perform_page_alloc(size_log2: u8, slot: SlotRefMut<'_, EmptySlot>) -> CapResult<()> {
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

fn perform_pgtbl_alloc(slot: SlotRefMut<'_, EmptySlot>) -> CapResult<()> {
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

    let _ = slot.replace(PgTbl::new(pgtbl));

    Ok(())
}

fn perform_thread_alloc(slot: SlotRefMut<'_, EmptySlot>) -> CapResult<()> {
    let thread = Thread::new(String::new(), None, None);

    let _ = slot.replace(thread);

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

    let private = thread.private.read();

    let root_hdr = &private
        .captbl
        .as_ref()
        .cloned()
        .ok_or(CapError::NotPresent)?;

    if from_page == into_pgtbl {
        return Err(CapError::InvalidOperation);
    }

    let mut from_page = root_hdr.get_mut::<Page>(from_page)?;
    let mut into_pgtbl = root_hdr.get_mut::<PgTbl>(into_pgtbl)?;

    into_pgtbl.map_page(&mut from_page, addr, flags)?;

    Ok(())
}

pub fn sys_thread_suspend(thread: &Arc<Thread>, thread_cap: u64) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let private = thread.private.read();
    let root_hdr = private.captbl.as_ref().unwrap();

    let thread_cap = root_hdr.get::<Arc<Thread>>(thread_cap)?;

    thread_cap.suspend();

    Ok(())
}

pub fn sys_thread_resume(thread: &Arc<Thread>, thread_cap: u64) -> CapResult<()> {
    let thread_cap = thread_cap as usize;
    let root_hdr = {
        let private = thread.private.read();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<Arc<Thread>>(thread_cap)?;

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
        let private = thread.private.read();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<Arc<Thread>>(thread_cap)?;
    let captbl = if captbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr
            .get::<Captbl>(captbl)?
            .upgrade()
            .ok_or(CapError::NotPresent)?
    };

    let pgtbl = if pgtbl == 0 {
        let private = thread.private.read();
        private.root_pgtbl.as_ref().unwrap().clone()
    } else {
        root_hdr.get::<PgTbl>(pgtbl)?.table().clone()
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
        let private = thread.private.read();
        private.captbl.as_ref().unwrap().clone()
    };

    let thread_cap = root_hdr.get::<Arc<Thread>>(thread_cap)?;

    thread_cap.write_registers(regs_vaddr)?;

    Ok(())
}

// pub fn sys_pgtbl_map(
//     private: &mut ProcPrivate,
//     from_pgtbl: u64,
//     into_pgtbl: u64,
//     vpn: Vpn,
//     flags: PageTableFlags,
// ) -> CapResult<()> {
//     let (from_pgtbl, into_pgtbl) = (from_pgtbl as usize, into_pgtbl as usize);

//     let root_hdr = &private.captbl;

//     if from_pgtbl == into_pgtbl {
//         return Err(CapError::InvalidOperation);
//     }

//     let mut from_pgtbl = root_hdr.get_mut::<PgTbl>(from_pgtbl)?;
//     let mut into_pgtbl = root_hdr.get_mut::<PgTbl>(into_pgtbl)?;

//     into_pgtbl.map_pgtbl(&mut from_pgtbl, vpn, flags)?;

//     Ok(())
// }
