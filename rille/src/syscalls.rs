//! Low-level types and constants for syscalls in river.

use num_enum::{FromPrimitive, IntoPrimitive};

/// Syscall numbers as provided to `ecall`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum SyscallNumber {
    /// [`Captr::<Untyped>::retype_many`][crate::capability::Captr::<Untyped>::retype_many]
    RetypeMany = 0,
    /// [`RemoteCaptr::<Captbl>::copy_deep`][crate::capability::RemoteCaptr::<Captbl>::copy_deep]
    CopyDeep = 1,
    /// [`RemoteCaptr::<Captbl>::delete`][crate::capability::RemoteCaptr::<Captbl>::delete]
    Delete = 2,
    /// [`RemoteCaptr::<Captbl>::swap`][crate::capability::RemoteCaptr::<Captbl>::swap]
    Swap = 3,
    /// [`Captr::<Page<L>>::map`][crate::capability::Captr::<Page<L>>::map`]
    PageMap = 4,
    /// [`Captr::<PageTable<L>>::map`][crate::capability::Captr::<PageTable<L>>::map`]
    PageTableMap = 5,
    /// [`Captr::<PageTable<L>>::map`][crate::capability::Captr::<PageTable<L>>::unmap`]
    PageTableUnmap = 6,
    /// TODO
    ThreadSuspend = 7,
    /// Print the debug representation of a capability to the kernel
    /// console.
    DebugCapSlot = 0xFFFF_0000,
    /// Print the debug representation of the root capabiltiy to the
    /// kernel console.
    DebugDumpRoot = 0xFFFF_0001,
    /// Print a string to the debug console.
    DebugPrint = 0xFFFF_0002,
    /// Identify the type of a capability.
    DebugCapIdentify = 0xFFFF_0003,
    /// Unknown syscall
    #[num_enum(default)]
    Unknown = u64::MAX,
}

macro_rules! impl_ecall {
    ($($(#[$meta:meta])? $name:ident => [$($arg_n:ident: $arg_n_reg:tt),*]);+) => {
	$(
	    $(#[$meta])?
	    ///
	    /// # Safety
	    ///
	    /// This function does a low-level `ecall`
	    /// instruction. The safety of this depends on the
	    /// specific syscall invoked, any may be undefined if the
	    /// syscall is undefined. Use with caution, and refer to
	    /// syscall documentation.
	    ///
	    /// # Errors
	    ///
	    /// Syscalls in river may return errors. For this reason,
	    /// this function returns a [`Result<u64, u64>`], where if
	    /// `reg_a0` (first return value register) was an error
	    /// (non-zero), the result is `Err(reg_a0)`. However, if
	    /// `reg_a0` was 0 (not an error), `Ok(reg_a1)` (second
	    /// return value register) is returned.
	    #[allow(clippy::too_many_arguments)]
	    #[inline]
	    pub unsafe fn $name(
		syscall: SyscallNumber,
		$(
		    $arg_n: u64
		),*
	    ) -> ::core::result::Result<u64, u64> {
		let error: u64;
		let value: u64;

		::core::arch::asm!(
		    "ecall",
		    inlateout("a0") u64::from(syscall) => error,
		    $(
			in($arg_n_reg) $arg_n,
		    )*
		    lateout("a1") value
		);

		match error {
		    0 => ::core::result::Result::Ok(value),
		    _ => ::core::result::Result::Err(error),
		}
	    }
	)+
    };
}

impl_ecall! {
    /// Perform a syscall with no arguments.
    ecall0 => [];
    /// Perform a syscall with 1 argument.
    ecall1 => [arg1: "a1"];
    /// Perform a syscall with 2 arguments.
    ecall2 => [arg1: "a1", arg2: "a2"];
    /// Perform a syscall with 3 arguments.
    ecall3 => [arg1: "a1", arg2: "a2", arg3: "a3"];
    /// Perform a syscall with 4 arguments.
    ecall4 => [arg1: "a1", arg2: "a2", arg3: "a3", arg4: "a4"];
    /// Perform a syscall with 5 arguments.
    ecall5 => [arg1: "a1", arg2: "a2", arg3: "a3", arg4: "a4", arg5: "a5"];
    /// Perform a syscall with 6 arguments.
    ecall6 => [arg1: "a1", arg2: "a2", arg3: "a3", arg4: "a4", arg5: "a5", arg6: "a6"];
    /// Perform a syscall with 7 arguments.
    ecall7 => [arg1: "a1", arg2: "a2", arg3: "a3", arg4: "a4", arg5: "a5", arg6: "a6", arg7: "a7"]
}

/// Syscalls relating to the [`Captbl`][super::capability::Captbl]
/// capability.
pub mod captbl {
    use crate::capability::{CapError, CapResult};

    use super::SyscallNumber;

    /// Copy a capability from one deeply nested slot to another
    /// deeply nested slot.
    ///
    /// See [`RemoteCaptr::<Captbl>::copy_deep`][1].
    ///
    /// # Description
    ///
    /// - `from_tbl_ref` may be a valid index of a [`Captbl`][2]
    /// capability in the thread's root captbl, or null. If null, this
    /// index is taken to be the thread's root captbl.
    ///
    /// - `from_tbl_index` must be a valid index of a [`Captbl`][2]
    /// capability in `from_tbl_ref`; or, if `from_tbl_ref` is null,
    /// the thread's root captbl.
    ///
    /// - `from_index` must be a valid empty slot in
    /// `from_tbl_ref[from_tbl_index]`; or, if the former is null,
    /// `from_tbl_index` itself.
    ///
    /// - `into_tbl_ref` may be a valid index of a [`Captbl`][2]
    /// capability in the thread's root captbl, or null. If null, this
    /// index is taken to be the thread's root captbl.
    ///
    /// - `into_tbl_index` must be a valid index of a [`Captbl`][2]
    /// capability in `into_tbl_ref`; or, if `into_tbl_ref` is null,
    /// the thread's root captbl.
    ///
    /// - `into_index` must be a valid empty slot in
    /// `into_tbl_ref[into_tbl_index]`; or, if the former is null,
    /// `into_tbl_index` itself.
    ///
    /// If these requirements are met, the capability described by the
    /// `from` indices will be copied (derived) with the exact same
    /// rights into the capability slot described by the `into`
    /// indices.
    ///
    /// # Errors
    ///
    /// If any capability was not present and was required,
    /// [`CapError::NotPresent`] will be returned. If any capability
    /// was of an invalid type, [`CapError::InvalidType`] is returned.
    ///
    /// [1]: crate::capability::RemoteCaptr::<Captbl>::copy_deep
    /// [2]: crate::capability::Captbl
    pub fn copy_deep(
        from_tbl_ref: usize,
        from_tbl_index: usize,
        from_index: usize,
        into_tbl_ref: usize,
        into_tbl_index: usize,
        into_index: usize,
    ) -> CapResult<()> {
        // SAFETY: copy_deep is always safe.
        let res = unsafe {
            super::ecall6(
                SyscallNumber::CopyDeep,
                from_tbl_ref as u64,
                from_tbl_index as u64,
                from_index as u64,
                into_tbl_ref as u64,
                into_tbl_index as u64,
                into_index as u64,
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn swap(
        cap1_table: usize,
        cap1_index: usize,
        cap2_table: usize,
        cap2_index: usize,
    ) -> CapResult<()> {
        // SAFETY: swap is always safe.
        let res = unsafe {
            super::ecall4(
                SyscallNumber::Swap,
                cap1_table as u64,
                cap1_index as u64,
                cap2_table as u64,
                cap2_index as u64,
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn delete(table: usize, index: usize) -> CapResult<()> {
        // SAFETY: delete is always safe.
        let res = unsafe { super::ecall2(SyscallNumber::Delete, table as u64, index as u64) };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }
}

/// Syscalls relating to the [`Untyped`][super::capability::Untyped]
/// capability.
#[allow(clippy::missing_errors_doc)]
pub mod untyped {
    use crate::capability::{CapError, CapResult, CapabilityType};

    use super::SyscallNumber;

    /// Allocate 1 or more capabilities from an [`Untyped`][2] capability.
    ///
    /// See [`Captr::<Untyped>::retype_many`][1].
    ///
    /// # Description
    ///
    /// - `untyped` must be a valid index to an [`Untyped`][2]
    /// capability in the thread's root captbl.
    ///
    /// - `into_ref` may be a valid index to a [`Captbl`][3], or
    /// null. If null, it is taken to be the thread's root captbl.
    ///
    /// - `into_index` must be a valid index to a [`Captbl`][3] in
    /// `into_ref`; or, if `into_ref` is null, the thread's root
    /// captbl.
    ///
    /// - `starting_at` must be a valid index to a range of empty
    /// capability slots, with length provided by `count`, in the
    /// captbl referred to by the combination of `into_ref` and
    /// `into_index`.
    ///
    /// - `count` must be more than 0.
    ///
    /// - `cap_type` must be a valid capability type.
    ///
    /// - `size` must contain valid size data for dynamically-sized
    /// capabilities.
    ///
    /// When these requirements are met, and kernel resources allow,
    /// the slots `into_ref[into_index][starting_at]` to
    /// `into_ref[into_index][starting_at + count]` are filled with
    /// new capabilities of the type requested.
    ///
    /// [1]: crate::capability::Captr::<Untyped>::retype_many
    /// [2]: crate::capability::Untyped
    /// [3]: crate::capability::Captbl
    pub fn retype_many(
        untyped: usize,
        into_ref: usize,
        into_index: usize,
        starting_at: usize,
        count: usize,
        cap_type: CapabilityType,
        size: usize,
    ) -> CapResult<()> {
        // SAFETY: retype_many is always safe.
        let res = unsafe {
            super::ecall7(
                SyscallNumber::RetypeMany,
                untyped as u64,
                into_ref as u64,
                into_index as u64,
                starting_at as u64,
                count as u64,
                u8::from(cap_type) as u64,
                size as u64,
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }
}

/// Syscalls for paging.
pub mod paging {
    use crate::{
        addr::{Identity, VirtualConst, Vpn},
        capability::{paging::PageTableFlags, CapError, CapResult},
    };

    use super::SyscallNumber;

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn pgtbl_map(
        from_pgtbl: usize,
        into_pgtbl: usize,
        vpn: Vpn,
        flags: PageTableFlags,
    ) -> CapResult<()> {
        // SAFETY: pgtbl_map is always safe.
        let res = unsafe {
            super::ecall4(
                SyscallNumber::PageTableMap,
                from_pgtbl as u64,
                into_pgtbl as u64,
                vpn.into_usize() as u64,
                flags.bits() as u64,
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn page_map(
        from_page: usize,
        into_pgtbl: usize,
        vaddr: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) -> CapResult<()> {
        // SAFETY: pgtbl_map is always safe.
        let res = unsafe {
            super::ecall4(
                SyscallNumber::PageMap,
                from_page as u64,
                into_pgtbl as u64,
                vaddr.into_usize() as u64,
                flags.bits() as u64,
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }
}

/// Syscalls for low-level debugging of capabilities.
pub mod debug {
    use crate::capability::{CapError, CapResult, CapabilityType};

    use super::SyscallNumber;

    /// Print the debug representation of a capability to the kernel
    /// console.
    ///
    /// # Description
    ///
    /// - `table` is the table to index into. If null, it is the root
    /// table.
    ///
    /// - `index` is a non-null index to a capability in `table`, or
    /// if `table` is null, the root table.
    ///
    /// # Errors
    ///
    /// - This function will return [`CapError::NotPresent`] if any
    /// capability index was out of bounds.
    ///
    /// - This function will return [`CapError::InvalidType`] if the
    /// `table` index did not refer to a capability table.
    pub fn debug_cap_slot(table: usize, index: usize) -> CapResult<()> {
        // SAFETY: debug_cap_slot is always safe.
        let res = unsafe { super::ecall2(SyscallNumber::DebugCapSlot, table as u64, index as u64) };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }

    /// Print the debug representation of this thread's root captbl to
    /// the kernel console.
    pub fn debug_dump_root() {
        // SAFETY: debug_dump_root is always safe.
        let _ = unsafe { super::ecall0(SyscallNumber::DebugDumpRoot) };

        // debug_dump_root will never fail.
    }

    /// Print a string to the kernel console.
    pub fn debug_print_string(s: &str) {
        // SAFETY: By invariants of &str.
        let _ =
            unsafe { super::ecall2(SyscallNumber::DebugPrint, s.as_ptr() as u64, s.len() as u64) };

        // print_string will never fail for valid `&str`s.
    }

    /// Get the capability type of a capability in the given slot.
    ///
    /// # Errors
    ///
    /// - This function will return [`CapError::NotPresent`] if any
    /// capability index was out of bounds.
    ///
    /// - This function will return [`CapError::InvalidType`] if the
    /// `table` index did not refer to a capability table.
    pub fn debug_cap_identify(table: usize, index: usize) -> CapResult<CapabilityType> {
        // SAFETY: debug_cap_identify is always safe
        let res =
            unsafe { super::ecall2(SyscallNumber::DebugCapIdentify, table as u64, index as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(v) => Ok(CapabilityType::from(v)),
        }
    }
}
