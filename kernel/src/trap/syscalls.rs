#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments
)]
use core::{ptr::NonNull, slice};

use rille::{
    addr::{Identity, VirtualConst},
    capability::{CapError, CapabilityType},
    units::StorageUnits,
};

use crate::{
    capability::{
        captbl::Captbl, slotref::SlotRefMut, untyped::Untyped, AnyCap, CapToOwned, CapabilitySlot,
        EmptySlot,
    },
    print, println,
    proc::ProcPrivate,
};

pub fn sys_debug_dump_root(private: &mut ProcPrivate) -> Result<(), CapError> {
    crate::println!("{:#x?}", private.captbl);
    Ok(())
}

pub fn sys_debug_cap_slot(private: &mut ProcPrivate, tbl: u64, index: u64) -> Result<(), CapError> {
    let (tbl, index) = (tbl as usize, index as usize);

    let root_hdr = &private.captbl;

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get::<Captbl>(tbl)?.to_owned_cap()
    };

    let slot = tbl.get::<AnyCap>(index)?;
    println!("{:#x?}", slot);

    Ok(())
}

pub fn sys_debug_print(
    private: &mut ProcPrivate,
    str_ptr: VirtualConst<u8, Identity>,
    str_len: u64,
) {
    let str_len = str_len as usize;

    assert!(
        str_ptr.add(str_len).into_usize() < private.mem_size,
        "too long"
    );

    // SAFETY: All values are valid for u8, and process memory is
    // zeroed and never uninitialized. TODO: make this not disclose
    // kernel memory
    let slice = unsafe {
        slice::from_raw_parts(
            private
                .mman
                .get_table()
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
    private: &mut ProcPrivate,
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

    let root_hdr = &private.captbl;

    let from_tbl_ref = if from_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get::<Captbl>(from_tbl_ref)?.to_owned_cap()
    };
    let from_tbl_index = from_tbl_ref.get::<Captbl>(from_tbl_index)?.to_owned_cap();
    drop(from_tbl_ref);

    let into_tbl_ref = if into_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get::<Captbl>(into_tbl_ref)?.to_owned_cap()
    };

    let into_tbl_index = into_tbl_ref.get::<Captbl>(into_tbl_index)?.to_owned_cap();
    drop(into_tbl_ref);

    if from_tbl_index == into_tbl_index && from_index == into_index {
        // Captrs are aliased, this is a no-op.
        return Ok(());
    }

    let into_index = into_tbl_index.get_mut::<EmptySlot>(into_index)?;
    let mut from_index = from_tbl_index.get_mut::<AnyCap>(from_index)?;

    let mut into_index = into_index.replace(from_index.to_owned_cap());
    from_index.add_child(&mut into_index);

    Ok(())
}

pub fn sys_swap(
    private: &mut ProcPrivate,
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

    let root_hdr = &private.captbl;

    if cap1_table == cap2_table && cap1_index == cap2_index {
        // Captrs are aliased, this is a no-op
        return Ok(());
    }

    let cap1_table = if cap1_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get(cap1_table)?.clone()
    };

    let cap2_table = if cap2_table == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get(cap2_table)?.clone()
    };

    let cap1 = cap1_table.get_mut::<AnyCap>(cap1_index)?;
    let cap2 = cap2_table.get_mut::<AnyCap>(cap2_index)?;

    cap1.swap(cap2)?;

    Ok(())
}

pub fn sys_retype_many(
    private: &mut ProcPrivate,
    untyped: u64,
    into_ref: u64,
    into_index: u64,
    starting_at: u64,
    count: u64,
    cap_type: CapabilityType,
    size: u64,
) -> Result<(), CapError> {
    let (untyped, into_ref, into_index, starting_at, count) = (
        untyped as usize,
        into_ref as usize,
        into_index as usize,
        starting_at as usize,
        count as usize,
    );
    let size = size as u8;

    let root_hdr = &private.captbl;

    // Check that we actually have an untyped cap, before we do more
    // processing.
    let untyped_ptr = {
        let untyped = root_hdr.get::<Untyped>(untyped)?;
        NonNull::new(untyped.as_ptr().cast_mut().cast())
    };

    let into_ref = if into_ref == 0 {
        root_hdr.clone()
    } else {
        root_hdr.get::<Captbl>(into_ref)?.to_owned_cap()
    };

    let into_index = into_ref.get::<Captbl>(into_index)?.to_owned_cap();

    if &into_index == root_hdr && (starting_at..starting_at + count).contains(&untyped) {
        // Captrs are aliased, this is invalid.
        return Err(CapError::InvalidOperation);
    }

    match cap_type {
        CapabilityType::Empty => {}
        CapabilityType::Captbl => {
            for i in 0..count {
                let mut untyped = root_hdr.get_mut::<Untyped>(untyped)?;
                let slot = into_index.get_mut::<EmptySlot>(starting_at + i)?;
                perform_captbl_retype(size, untyped_ptr, &mut untyped, slot)?;
            }
        }
        CapabilityType::Unknown => return Err(CapError::InvalidOperation),
        _ => todo!(),
    }

    Ok(())
}

fn perform_captbl_retype(
    n_slots_log2: u8,
    untyped_ptr: Option<NonNull<CapabilitySlot>>,
    untyped: &'_ mut SlotRefMut<'_, Untyped>,
    slot: SlotRefMut<'_, EmptySlot>,
) -> Result<(), CapError> {
    let size_bytes_log2 = n_slots_log2 + 6;
    if !(12..=64).contains(&size_bytes_log2) {
        return Err(CapError::InvalidSize);
    }
    let size_bytes = 1 << size_bytes_log2;

    let ptr = untyped.free_addr();
    let ptr = if ptr.is_page_aligned() {
        ptr
    } else {
        ptr.add(ptr.into_usize() % 4.kib())
    };

    if ptr.add(size_bytes) >= untyped.base_addr().add(1 << untyped.size_log2()) {
        return Err(CapError::NotEnoughResources);
    }

    untyped.set_free_addr(ptr.add(size_bytes));

    // SAFETY: This untyped is still valid, as we hold its
    // lock. Additionally, this captbl is valid as the memory has been
    // zeroed (by the invariants of Untyped), and is of the proper
    // size.
    let _ = slot.replace(unsafe { Captbl::new(ptr.into_virt(), n_slots_log2, untyped_ptr) });

    Ok(())
}
