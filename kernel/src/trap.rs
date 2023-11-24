use core::{arch::global_asm, mem, ptr};

use crate::{
    asm::{self, hartid, intr_enabled, SCAUSE_INTR_BIT, SSTATUS_SPP},
    capability::{Thread, ThreadState, THREAD_STACK_SIZE},
    hart_local::LOCAL_HART,
    paging::Satp,
    plic::PLIC,
    sync::OnceCell,
    trampoline::{self, trampoline, Trapframe},
    uart,
};
use atomic::Ordering;
use rille::{
    capability::{CapError, CapabilityType},
    syscalls::SyscallNumber,
};

mod syscalls;

pub struct Irqs {
    pub uart: u32,
}

pub static IRQS: OnceCell<Irqs> = OnceCell::new();

#[no_mangle]
unsafe extern "C" fn kernel_trap() {
    assert!(!intr_enabled(), "kernel_trap: interrupts enabled");
    // N.B. LOCAL_HART is not SpinMutex-guarded, so interrupts will not be
    // enabled during this line. And we catch if they were above.
    //
    // This value helps to make sure that `push_off` and `pop_off` don't
    // accidentally re-enable interrupts during our handler by way of a
    // `SpinMutexGuard` drop. (I originally tried to just set intena, but it was
    // still enough of a race condition that it caused bursts of the same
    // interrupt--not sure entirely why, this is a bit hacky but I suppose it
    // works)
    LOCAL_HART.with(|hart| hart.trap.set(true));

    let sepc = asm::read_sepc();
    let sstatus = asm::read_sstatus();
    let scause = asm::read_scause();

    debug_assert_ne!(
        sstatus & SSTATUS_SPP,
        0,
        "kernel_trap: did not trap from S-mode, sstatus={sstatus}",
    );

    // Not an interrupt.
    assert!(
        scause & SCAUSE_INTR_BIT != 0,
        "kernel exception: {}, hart={} sepc={:#x} stval={:#x} scause={:#x}",
        describe_exception(scause),
        hartid(),
        sepc,
        asm::read_stval(),
        scause
    );

    let kind = device_interrupt(scause);
    assert!(
        kind != InterruptKind::Unknown,
        "kernel trap from unknown device, sepc={:#x} stval={:#x} scause={:#x}",
        sepc,
        asm::read_stval(),
        scause
    );

    {
        let proc = LOCAL_HART.with(|hart| {
            if kind == InterruptKind::Timer {
                hart.thread.borrow().as_ref().cloned()
            } else {
                None
            }
        });

        if let Some(proc) = proc {
            if proc.state.load(Ordering::Relaxed) == ThreadState::Running {
                proc.state.store(ThreadState::Runnable, Ordering::Relaxed);
                asm::write_sepc(sepc);
                asm::write_sstatus(sstatus);
                // Make sure we inform the HartCtx that it's fine to re-enable interrupts
                // now.
                LOCAL_HART.with(|hart| hart.trap.set(false));
                proc.state.store(ThreadState::Runnable, Ordering::Relaxed);
                drop(proc);
                // SAFETY: This thread is currently running on
                // this hart.
                unsafe {
                    Thread::yield_to_scheduler();
                }
            }
        }
    }

    asm::write_sepc(sepc);
    asm::write_sstatus(sstatus);
    // Make sure we inform the HartCtx that it's fine to re-enable interrupts
    // now.
    LOCAL_HART.with(|hart| hart.trap.set(false));
}

fn device_interrupt(scause: u64) -> InterruptKind {
    // top bit => supervisor interrupt via PLIC
    // cause == 9 => external interrut
    if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 9 {
        // claim this interrupt
        let irq = PLIC.hart_sclaim();

        if irq == IRQS.expect("IRQS").uart {
            uart::handle_interrupt();
        } else if irq == 0x0 {
            // FIXME: I have no idea what the hell this means and why
            // it's being triggered here, but... sure it's fine
            return InterruptKind::External;
        } else {
            panic!("device_interrupt: unexpected IRQ {}", irq);
        }

        if irq != 0 {
            PLIC.hart_sunclaim(irq);
        }

        InterruptKind::External
    } else if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 5 {
        asm::timer_intr_clear();

        let interval = LOCAL_HART.with(|hart| hart.timer_interval.get());
        sbi::timer::set_timer(asm::read_time() + interval).unwrap();
        asm::timer_intr_on();

        InterruptKind::Timer
    } else {
        InterruptKind::Unknown
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InterruptKind {
    Software,
    Timer,
    External,
    Unknown,
}

extern "C" {
    pub fn kernel_trapvec();
}

global_asm!("
# stolen from xv6-riscv, licensed under MIT:
# https://github.com/mit-pdos/xv6-riscv/blob/f5b93ef12f7159f74f80f94729ee4faabe42c360/kernel/kernelvec.S
.pushsection .init.trapvec,\"ax\",@progbits

.globl kernel_trap
.type kernel_trap, @function

.globl kernel_trapvec
.type kernel_trapvec, @function

.align 4
# interrupts and exceptions in S-mode call this handler. the stack here is
# **always** a kernel stack, so it's safe to trust tp and similar kernel-memory
# GPRs.
kernel_trapvec:
    # make room to save registers
    addi sp, sp, -256

    # save the registers.
    sd ra,    0(sp)
    sd sp,    8(sp)
    sd gp,   16(sp)
    sd tp,   24(sp)
    sd t0,   32(sp)
    sd t1,   40(sp)
    sd t2,   48(sp)
    sd s0,   56(sp)
    sd s1,   64(sp)
    sd a0,   72(sp)
    sd a1,   80(sp)
    sd a2,   88(sp)
    sd a3,   96(sp)
    sd a4,  104(sp)
    sd a5,  112(sp)
    sd a6,  120(sp)
    sd a7,  128(sp)
    sd s2,  136(sp)
    sd s3,  144(sp)
    sd s4,  152(sp)
    sd s5,  160(sp)
    sd s6,  168(sp)
    sd s7,  176(sp)
    sd s8,  184(sp)
    sd s9,  192(sp)
    sd s10, 200(sp)
    sd s11, 208(sp)
    sd t3,  216(sp)
    sd t4,  224(sp)
    sd t5,  232(sp)
    sd t6,  240(sp)

    # call the trap handler in src/trap.rs
    call {kernel_trap}

    # restore registers.
    ld ra,    0(sp)
    ld sp,    8(sp)
    ld gp,   16(sp)
    # not tp (contains hart-local data ptr), in case we moved CPUs
    ld t0,   32(sp)
    ld t1,   40(sp)
    ld t2,   48(sp)
    ld s0,   56(sp)
    ld s1,   64(sp)
    ld a0,   72(sp)
    ld a1,   80(sp)
    ld a2,   88(sp)
    ld a3,   96(sp)
    ld a4,  104(sp)
    ld a5,  112(sp)
    ld a6,  120(sp)
    ld a7,  128(sp)
    ld s2,  136(sp)
    ld s3,  144(sp)
    ld s4,  152(sp)
    ld s5,  160(sp)
    ld s6,  168(sp)
    ld s7,  176(sp)
    ld s8,  184(sp)
    ld s9,  192(sp)
    ld s10, 200(sp)
    ld s11, 208(sp)
    ld t3,  216(sp)
    ld t4,  224(sp)
    ld t5,  232(sp)
    ld t6,  240(sp)

    addi sp, sp, 256

    # return to whatever we were doing in the kernel.
    sret

.popsection
", kernel_trap = sym kernel_trap);

pub fn describe_exception(id: u64) -> &'static str {
    match id {
        0 => "insn addr misaligned",
        1 => "insn addr fault",
        2 => "illegal insn",
        3 => "breakpoint",
        4 => "load addr misaligned",
        5 => "load access fault",
        6 => "store/AMO addr misaligned",
        7 => "store/AMO access fault",
        8 => "U-mode envcall",
        9 => "S-mode envcall",
        12 => "insn page fault",
        13 => "load page fault",
        15 => "store/AMO page fault",
        10 | 11 | 14 | 16..=23 | 32..=47 | 64.. => "reserved",
        24..=31 | 48..=63 => "unknown",
    }
}

#[link_section = ".init.user_trap"]
unsafe extern "C" fn user_trap() -> ! {
    // Make sure we interrupt into kernel_trapvec, now that we're in S-mode.
    // SAFETY: The address we provide is valid.
    unsafe { asm::write_stvec(kernel_trapvec as *const u8) }

    assert!(
        asm::read_sstatus() & SSTATUS_SPP == 0,
        "user_trap: Did not trap from U-mode, sstatus={}",
        asm::read_sstatus()
    );

    {
        let proc = LOCAL_HART.with(|hart| {
            hart.trap.set(true);

            hart.thread.borrow().as_ref().cloned().unwrap()
        });

        // We cannot deadlock here. Full stop. Here's why:
        // 1. We are the user trap handler. If this lock was held by
        // the same thread, then something has gone wrong--we can't
        // access this datastructure from U-mode.
        // 2. Even if there were any instances of this lock held
        // before, we wouldn't have been able to interrupt unless
        // someone enabled interrupts while the lock was still held
        // (which is a bug itself).
        let mut trapframe_lock = proc.trapframe.lock();
        let trapframe = trapframe_lock.as_mut();
        trapframe.user_epc = asm::read_sepc();

        let scause = asm::read_scause();
        // Exception, not a syscall or interrupt.
        assert!(
            scause & SCAUSE_INTR_BIT != 0 || scause == 8,
            "user exception: {}, hart={} pid={} sepc={:#x} stval={:#x} scause={:#x}",
            describe_exception(scause),
            hartid(),
            proc.tid,
            asm::read_sepc(),
            asm::read_stval(),
            scause
        );

        if scause == 8 {
            // syscall

            // `ecall` instrs are 4 bytes, skip to the instruction after
            trapframe.user_epc += 4;

            match trapframe.a0.into() {
                SyscallNumber::CopyDeep => sys_copy_deep(&proc, trapframe),
                SyscallNumber::AllocateMany => sys_allocate_many(&proc, trapframe),
                SyscallNumber::DebugCapSlot => sys_debug_cap_slot(&proc, trapframe),
                SyscallNumber::DebugDumpRoot => sys_debug_dump_root(&proc, trapframe),
                SyscallNumber::DebugPrint => sys_debug_print(&proc, trapframe),
                SyscallNumber::Swap => sys_swap(&proc, trapframe),
                SyscallNumber::Delete => sys_delete(&proc, trapframe),
                // SyscallNumber::PageTableMap => sys_pgtbl_map(&mut private, trapframe),
                SyscallNumber::PageMap => sys_page_map(&proc, trapframe),
                SyscallNumber::DebugCapIdentify => {
                    sys_debug_cap_identify(&proc, trapframe);
                }
                SyscallNumber::ThreadSuspend => sys_thread_suspend(&proc, trapframe),
                SyscallNumber::ThreadConfigure => sys_thread_configure(&proc, trapframe),
                SyscallNumber::ThreadResume => sys_thread_resume(&proc, trapframe),
                SyscallNumber::ThreadWriteRegisters => {
                    sys_thread_write_registers(&proc, trapframe);
                }
                _ => {
                    trapframe.a0 = CapError::InvalidOperation.into();
                }
            }

            if proc.state.load(Ordering::Acquire) == ThreadState::Suspended {
                drop(trapframe_lock);
                LOCAL_HART.with(|hart| hart.trap.set(false));
                // SAFETY: This thread is currently running on this hart.
                unsafe {
                    proc.yield_to_scheduler_final();
                }
            }

            // println!(
            //     "user trap! a0={} epc={:#x}",
            //     trapframe.a0, trapframe.user_epc
            // );
        } else {
            let kind = device_interrupt(scause);
            assert!(
                kind != InterruptKind::Unknown,
                "user trap from unknown device, pid={} sepc={:#x} stval={:#x} scause={:#x}",
                proc.tid,
                trapframe.user_epc,
                asm::read_stval(),
                scause
            );
            if proc.state.load(Ordering::Acquire) == ThreadState::Suspended {
                drop(trapframe_lock);
                LOCAL_HART.with(|hart| hart.trap.set(false));
                // SAFETY: This thread is currently running on this hart.
                unsafe {
                    proc.yield_to_scheduler_final();
                }
            } else if kind == InterruptKind::Timer {
                // Make sure we don't hold any locks while we context
                // switch.
                drop(trapframe_lock);
                proc.state.store(ThreadState::Runnable, Ordering::Relaxed);
                LOCAL_HART.with(|hart| hart.trap.set(false));
                drop(proc);
                // SAFETY: This thread is currently running on this hart.
                unsafe {
                    Thread::yield_to_scheduler();
                }
            }
        }
    } // guards: trapframe

    LOCAL_HART.with(|hart| hart.trap.set(false));
    // SAFETY: We are calling this function in the context of a valid
    // process.
    unsafe { user_trap_ret() }
}

/// # Safety
///
/// This function must be called within the context of a
/// valid process.
///
/// # Panics
///
/// This function will panic if there is not a process scheduled on
/// the local hart.
#[inline(never)]
pub unsafe extern "C" fn user_trap_ret() -> ! {
    asm::intr_off();
    LOCAL_HART.with(|hart| hart.trap.set(true));

    // SAFETY: The address we provide is valid.
    unsafe { asm::write_stvec(trampoline() as *const u8) };

    let satp = LOCAL_HART.with(|hart| {
        let proc = hart.thread.borrow().as_ref().cloned().unwrap();

        let private = proc.private.read();

        // SAFETY: Our caller guarantees that we are within a valid
        // process's context.

        let mut trapframe = proc.trapframe.lock();
        trapframe.as_mut().kernel_satp = asm::get_satp().as_usize() as u64;
        trapframe.as_mut().kernel_sp =
            proc.stack.as_phys().into_virt().into_usize() as u64 + THREAD_STACK_SIZE as u64;
        trapframe.as_mut().kernel_trap = user_trap as usize as u64;
        trapframe.as_mut().kernel_tp = asm::tp();

        asm::write_sepc(trapframe.as_ref().user_epc);
        asm::write_sscratch(private.trapframe_addr as u64);
        let satp = Satp {
            asid: 1,
            ppn: private
                .root_pgtbl
                .as_ref()
                .unwrap()
                .as_physical_const()
                .ppn(),
        };
        satp.encode().as_usize()
    });

    // Enable interrupts in user mode
    asm::set_spie();
    // Clear SPP = user mode
    asm::clear_spp();

    // SAFETY: The function pointer type and address are valid.
    let ret_user = unsafe {
        mem::transmute::<usize, unsafe extern "C" fn(usize) -> !>(trampoline::ret_user())
    };
    LOCAL_HART.with(|hart| hart.trap.set(false));

    // SAFETY: The function address is valid and we have provided it with the proper values.
    unsafe { (ret_user)(satp) }
}

trait FromTrapframe
where
    Self: Sized,
{
    fn from_trapframe(frame: &Trapframe) -> Self;
}
macro_rules! impl_from_trapframe {
    ($($($ty:ident: $reg:ident),+);*) => {
	$(
	    impl<$($ty: From<u64>,)+> FromTrapframe for ($($ty,)+) {
		fn from_trapframe(frame: &Trapframe) -> Self {
		    ($($ty::from(frame.$reg),)+)
		}
	    }
	)*
    }
}

impl_from_trapframe! {
    T1: a1;
    T1: a1, T2: a2;
    T1: a1, T2: a2, T3: a3;
    T1: a1, T2: a2, T3: a3, T4: a4;
    T1: a1, T2: a2, T3: a3, T4: a4, T5: a5;
    T1: a1, T2: a2, T3: a3, T4: a4, T5: a5, T6: a6;
    T1: a1, T2: a2, T3: a3, T4: a4, T5: a5, T6: a6, T7: a7
}

trait IntoTrapframe
where
    Self: Sized,
{
    fn into_trapframe(self) -> (u64, u64);
}

impl<T: IntoTrapframe, E: IntoTrapframe> IntoTrapframe for Result<T, E> {
    fn into_trapframe(self) -> (u64, u64) {
        match self {
            Ok(v) => (0, v.into_trapframe().0),
            Err(e) => (e.into_trapframe().0, 0),
        }
    }
}

impl IntoTrapframe for () {
    fn into_trapframe(self) -> (u64, u64) {
        (0, 0)
    }
}

impl IntoTrapframe for u64 {
    fn into_trapframe(self) -> (u64, u64) {
        (self, 0)
    }
}

impl IntoTrapframe for CapError {
    fn into_trapframe(self) -> (u64, u64) {
        (self.into(), 0)
    }
}

impl IntoTrapframe for CapabilityType {
    fn into_trapframe(self) -> (u64, u64) {
        (self.into(), 0)
    }
}

macro_rules! define_syscall {
    ($ident:ident, $arity:tt) => {
	fn $ident(private: &::alloc::sync::Arc<$crate::capability::Thread>, trapframe: &mut $crate::trampoline::Trapframe) {
	    define_syscall!(@arity private, trapframe, $ident, $arity);
	}
    };

    (@arity $private:ident, $trapframe:ident, $ident:ident, 7) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	    arg7,
	) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	    arg7,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 6) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 5) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 4) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 3) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (arg1, arg2, arg3) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	    arg3,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 2) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (arg1,arg2) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident(
	    $private,
	    arg1,
	    arg2,
	).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 1) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (arg1,) = FromTrapframe::from_trapframe(&$trapframe);
	let (a0, a1) = syscalls::$ident($private, arg1).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
    (@arity $private:ident, $trapframe:ident, $ident:ident, 0) => {{
	use $crate::trap::IntoTrapframe;
	let (a0, a1) = syscalls::$ident($private).into_trapframe();
	$trapframe.a0 = a0;
	$trapframe.a1 = a1;
    }};
}

define_syscall!(sys_debug_dump_root, 0);
define_syscall!(sys_debug_cap_slot, 2);
define_syscall!(sys_debug_print, 2);
define_syscall!(sys_copy_deep, 6);
define_syscall!(sys_swap, 4);
define_syscall!(sys_allocate_many, 7);
define_syscall!(sys_delete, 2);
define_syscall!(sys_page_map, 4);
// define_syscall!(sys_pgtbl_map, 4);
define_syscall!(sys_debug_cap_identify, 2);
define_syscall!(sys_thread_suspend, 1);
define_syscall!(sys_thread_configure, 3);
define_syscall!(sys_thread_resume, 1);
define_syscall!(sys_thread_write_registers, 2);
