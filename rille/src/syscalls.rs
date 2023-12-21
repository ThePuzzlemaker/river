//! Low-level types and constants for syscalls in river.

use num_enum::{FromPrimitive, IntoPrimitive};

/// Syscall numbers as provided to `ecall`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum SyscallNumber {
    /// [`RemoteCaptr::<Captbl>::delete`][crate::capability::RemoteCaptr::<Captbl>::delete]
    CaptrDelete = 0,
    /// [`Page<L>::create`][crate::capability::paging::Page<L>::create]
    PageCreate,
    /// [`Captr::<Page<L>>::map`][crate::capability::Captr::<Page<L>>::map]
    PageMap,
    /// [`PageTable::create`][crate::capability::paging::PageTable::create]
    PageTableCreate,
    /// [`Captr::<PageTable>::unmap`][crate::capability::Captr::<PageTable>::unmap]
    PageTableUnmap,
    /// [`Captr::<Thread>::suspend`][crate::capability::Captr::<Thread>::suspend]
    ThreadSuspend,
    /// [`Captr::<Thread>::configure`][crate::capability::Captr::<Thread>::configure]
    ThreadConfigure,
    /// [`Captr::<Thread>::resume`][crate::capability::Captr::<Thread>::resume]
    ThreadResume,
    /// [`Captr::<Thread>::write_registers`][crate::capability::Captr::<Thread>::write_registers]
    ThreadWriteRegisters,
    ThreadStart,
    /// TODO
    CaptrGrant,
    /// [`Notification::create`][crate::capability::Notification::create]
    NotificationCreate,
    /// [`Notification::wait`][crate::capability::Notification::wait]
    NotificationWait,
    /// [`Notification::signal`][crate::capability::Notification::signal]
    NotificationSignal,
    /// [`Notification::poll`][crate::capability::Notification::poll]
    NotificationPoll,
    /// TODO
    Yield,
    /// TODO
    IntrHandlerAck,
    /// TODO
    IntrHandlerBind,
    /// TODO
    IntrHandlerUnbind,
    /// TODO
    IntrPoolGet,
    /// TODO
    EndpointSend,
    /// TODO
    EndpointRecv,
    /// TODO
    ThreadSetPriority,
    /// TODO
    ThreadSetIpcBuffer,
    // [`Endpoint::create`][crate::capability::Endpoint::create]
    EndpointCreate,
    /// TODO
    EndpointReply,
    /// TODO
    EndpointCall,
    /// [`Job::create`][crate::capability::Job::create]
    JobCreate,
    /// [`Job::link`][crate::capability::Job::link]
    JobLink,
    /// [`Job::create_thread`][crate::capability::Job::create_thread]
    JobCreateThread,
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

/// Syscalls available for all [`Captr`][super::capability::Captr]s.
pub mod captr {
    use crate::capability::{CapError, CapResult};

    use super::SyscallNumber;

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn delete(index: usize) -> CapResult<()> {
        // SAFETY: delete is always safe.
        let res = unsafe { super::ecall1(SyscallNumber::CaptrDelete, index as u64) };

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
    pub fn pgtbl_unmap(from_pgtbl: usize, vpn: Vpn) -> CapResult<()> {
        // SAFETY: pgtbl_map is always safe.
        let res = unsafe {
            super::ecall2(
                SyscallNumber::PageTableUnmap,
                from_pgtbl as u64,
                vpn.into_usize() as u64,
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
    /// # Errors
    ///
    /// - This function will return [`CapError::NotPresent`] if any
    /// capability index was out of bounds.
    pub fn debug_cap_slot(index: usize) -> CapResult<()> {
        // SAFETY: debug_cap_slot is always safe.
        let res = unsafe { super::ecall1(SyscallNumber::DebugCapSlot, index as u64) };

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
    pub fn debug_cap_identify(index: usize) -> CapResult<CapabilityType> {
        // SAFETY: debug_cap_identify is always safe
        let res = unsafe { super::ecall1(SyscallNumber::DebugCapIdentify, index as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(v) => Ok(CapabilityType::from(v)),
        }
    }
}

/// Syscalls relating to threads. See the [`Thread`][1] capability for
/// more details.
///
/// [1]: crate::capability::Thread
pub mod thread {
    #[allow(unused_imports)]
    use crate::capability::Captr;
    use crate::capability::{CapError, CapResult, UserRegisters};

    use super::SyscallNumber;

    /// See [`Captr::<Thread>::suspend`].
    #[allow(clippy::missing_errors_doc)]
    pub fn suspend(thread: usize) -> CapResult<()> {
        // SAFETY: thread_suspend is always safe
        let res = unsafe { super::ecall1(SyscallNumber::ThreadSuspend, thread as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(_) => Ok(()),
        }
    }

    /// See [`Captr::<Thread>::resume`].
    #[allow(clippy::missing_errors_doc, clippy::missing_safety_doc)]
    pub unsafe fn resume(thread: usize) -> CapResult<()> {
        // SAFETY: By invariants.
        let res = unsafe { super::ecall1(SyscallNumber::ThreadResume, thread as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(_) => Ok(()),
        }
    }

    /// See [`Captr::<Thread>::configure`].
    #[allow(clippy::missing_errors_doc, clippy::missing_safety_doc)]
    pub unsafe fn configure(thread: usize, pgtbl: usize, ipc_buffer: usize) -> CapResult<()> {
        // SAFETY: By invariants.
        let res = unsafe {
            super::ecall3(
                SyscallNumber::ThreadConfigure,
                thread as u64,
                pgtbl as u64,
                ipc_buffer as u64,
            )
        };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(_) => Ok(()),
        }
    }

    /// See [`Captr::<Thread>::write_registers`].
    #[allow(clippy::missing_errors_doc, clippy::missing_safety_doc)]
    pub unsafe fn write_registers(thread: usize, registers: *const UserRegisters) -> CapResult<()> {
        // SAFETY: By invariants.
        let res = unsafe {
            super::ecall2(
                SyscallNumber::ThreadWriteRegisters,
                thread as u64,
                registers as u64,
            )
        };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(_) => Ok(()),
        }
    }
}

/// Syscalls relating to notifications. See the [`Notification`][1]
/// capability for more details.
///
/// [1]: crate::capability::Notification
pub mod notification {
    use core::num::NonZeroU64;

    #[allow(unused_imports)]
    use crate::capability::Captr;
    use crate::capability::{CapError, CapResult};

    use super::SyscallNumber;

    /// See [`Captr::<Notification>::wait`].
    #[allow(clippy::missing_errors_doc)]
    pub fn wait(notification: usize) -> CapResult<Option<NonZeroU64>> {
        // SAFETY: notification_wait is always safe
        let res = unsafe { super::ecall1(SyscallNumber::NotificationWait, notification as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(v) => Ok(NonZeroU64::new(v)),
        }
    }

    /// See [`Captr::<Notification>::signal`].
    #[allow(clippy::missing_errors_doc, clippy::missing_safety_doc)]
    pub fn signal(notification: usize) -> CapResult<()> {
        // SAFETY: notification_signal is always safe.
        let res = unsafe { super::ecall1(SyscallNumber::NotificationSignal, notification as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(_) => Ok(()),
        }
    }

    /// See [`Captr::<Notification>::poll`].
    #[allow(clippy::missing_errors_doc, clippy::missing_safety_doc)]
    pub fn poll(notification: usize) -> CapResult<Option<NonZeroU64>> {
        // SAFETY: notification_poll is always safe.
        let res = unsafe { super::ecall1(SyscallNumber::NotificationPoll, notification as u64) };

        match res {
            Err(e) => Err(CapError::from(e)),
            Ok(v) => Ok(NonZeroU64::new(v)),
        }
    }
}
