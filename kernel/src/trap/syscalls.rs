#![allow(
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    clippy::too_many_arguments
)]
use core::slice;

use rille::{
    addr::{Identity, VirtualConst},
    capability::{CapError, CapabilityType},
    units::StorageUnits,
};

use crate::{
    capability::{
        captbl::Captbl, slotref::SlotRefMut, untyped::Untyped, AnyCap, CapToOwned, EmptySlot,
        SlotPtrWithTable,
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
    let root_lock = root_hdr.read();

    let tbl = if tbl == 0 {
        root_hdr.clone()
    } else {
        root_lock.get(tbl)?.clone()
    };
    let tbl = tbl.read();

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
        let root_lock = root_hdr.read();
        root_lock.get::<Captbl>(from_tbl_ref)?.to_owned_cap()
    };
    let from_tbl_ref = from_tbl_ref.read();
    let from_tbl_index = from_tbl_ref.get::<Captbl>(from_tbl_index)?.to_owned_cap();
    drop(from_tbl_ref);

    let from_tbl_index_lock = from_tbl_index.read();

    let into_tbl_ref = if into_tbl_ref == 0 {
        root_hdr.clone()
    } else {
        let root_lock = root_hdr.read();
        root_lock.get::<Captbl>(into_tbl_ref)?.to_owned_cap()
    };
    let into_tbl_ref = into_tbl_ref.read();

    let into_tbl_index = into_tbl_ref.get::<Captbl>(into_tbl_index)?.to_owned_cap();
    drop(into_tbl_ref);

    let mut from_tbl_index_lock = from_tbl_index_lock.upgrade();
    if from_tbl_index == into_tbl_index {
        // Captrs are aliased, make sure we don't deadlock
        let (mut into_index, mut from_index) =
            from_tbl_index_lock.get2_mut::<EmptySlot, AnyCap>(into_index, from_index)?;

        let mut into_index = into_index.replace(from_index.to_owned_cap());
        from_index.add_child(&mut into_index);
    } else {
        let mut into_tbl_index = into_tbl_index.write();

        let mut into_index = into_tbl_index.get_mut::<EmptySlot>(into_index)?;
        let mut from_index = from_tbl_index_lock.get_mut::<AnyCap>(from_index)?;

        let mut into_index = into_index.replace(from_index.to_owned_cap());
        from_index.add_child(&mut into_index);
    }

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
        let root_lock = root_hdr.read();
        let untyped = root_lock.get::<Untyped>(untyped)?;
        SlotPtrWithTable::new(untyped.as_ptr(), untyped.table_ptr())
    };

    let into_index = {
        let into_ref = if into_ref == 0 {
            root_hdr.clone()
        } else {
            let root_lock = root_hdr.read();
            root_lock.get::<Captbl>(into_ref)?.to_owned_cap()
        };
        let into_ref = into_ref.read();

        into_ref.get::<Captbl>(into_index)?.to_owned_cap()
    };

    // Make sure the captrs aren't aliased--if they are, use get2_mut
    // to not deadlock.
    if into_index == *root_hdr {
        let mut root_lock = root_hdr.write();

        match cap_type {
            CapabilityType::Empty => {}
            CapabilityType::Captbl => {
                for i in 0..count {
                    let (mut untyped, mut slot) =
                        root_lock.get2_mut::<Untyped, EmptySlot>(untyped, starting_at + i)?;
                    perform_captbl_retype(size, untyped_ptr, &mut untyped, &mut slot)?;
                }
            }
            CapabilityType::Untyped => todo!(),
            CapabilityType::PgTbl => todo!(),
            CapabilityType::Page => todo!(),
            CapabilityType::Unknown => return Err(CapError::InvalidOperation),
        }
    } else {
        match cap_type {
            CapabilityType::Empty => {}
            CapabilityType::Captbl => {
                for i in 0..count {
                    let mut into_lock = into_index.write();
                    let mut root_lock = root_hdr.write();

                    let mut untyped = root_lock.get_mut::<Untyped>(untyped)?;
                    let mut slot = into_lock.get_mut::<EmptySlot>(starting_at + i)?;
                    perform_captbl_retype(size, untyped_ptr, &mut untyped, &mut slot)?;
                }
            }
            CapabilityType::Untyped => todo!(),
            CapabilityType::PgTbl => todo!(),
            CapabilityType::Page => todo!(),
            CapabilityType::Unknown => return Err(CapError::InvalidOperation),
        }
    }

    Ok(())
}

fn perform_captbl_retype(
    n_slots_log2: u8,
    untyped_ptr: SlotPtrWithTable,
    untyped: &'_ mut SlotRefMut<'_, Untyped>,
    slot: &'_ mut SlotRefMut<'_, EmptySlot>,
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
