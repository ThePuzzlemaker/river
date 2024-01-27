use core::{arch::global_asm, mem, sync::atomic::AtomicUsize};

use crate::{
    asm::{self, hartid, InterruptDisabler, SCAUSE_INTR_BIT, SSTATUS_SPP},
    capability::{global_interrupt_pool, Thread, ThreadState, THREAD_STACK_SIZE},
    hart_local::{CURRENT_ASID, LOCAL_HART},
    paging::Satp,
    plic::PLIC,
    sched::Scheduler,
    sync::{OnceCell, SpinMutex},
    trampoline::{self, trampoline, Trapframe},
    MAX_HARTS,
};
use atomic::Ordering;
use rille::{
    capability::{CapError, CapabilityType, MessageHeader},
    syscalls::SyscallNumber,
};

mod syscalls;

pub struct Irqs {
    pub uart: u32,
}

pub static IRQS: OnceCell<Irqs> = OnceCell::new();

#[no_mangle]
unsafe extern "C" fn kernel_trap() {
    debug_assert!(!asm::intr_off(), "kernel_trap: interrupts enabled");
    let mut intr = InterruptDisabler::new();

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
        let proc = if kind == InterruptKind::Timer || kind == InterruptKind::Software {
            LOCAL_HART.thread.borrow().as_ref().cloned()
        } else {
            None
        };

        if let Some(proc) = proc {
            let mut private = proc.private.lock();

            let state = private.state;
            if state == ThreadState::Running {
                asm::write_sepc(sepc);
                asm::write_sstatus(sstatus);

                if private.prio.effective() < 0 {
                    private.state = ThreadState::Runnable;
                }
                // Never reschedule the idle process.
                if private.prio.effective() >= 0 && state != ThreadState::Blocking {
                    if kind == InterruptKind::Timer {
                        //Scheduler::debug();
                        //crate::println!("=====^before^=====");
                        let old_prio = private.prio.effective();
                        Scheduler::dequeue_dl(&proc, &mut private);
                        proc.prio_diminish_dl(&mut private);
                        if private.prio.effective() > old_prio {
                            Scheduler::enqueue_front_dl(&proc, Some(hartid()), &mut private);
                        } else {
                            Scheduler::enqueue_dl(&proc, Some(hartid()), &mut private);
                        }
                        //Scheduler::debug();
                        //crate::println!("=====^after^=====");
                        private.state = ThreadState::Runnable;
                    } else if kind == InterruptKind::Software && state == ThreadState::Running {
                        private.state = ThreadState::Runnable;
                    }
                }

                drop(private);
                drop(proc);
                drop(intr);
                // SAFETY: This thread is currently running on
                // this hart.
                unsafe {
                    Thread::yield_to_scheduler();
                }
                intr = InterruptDisabler::new();
            }
        }
    }

    asm::write_sepc(sepc);
    asm::write_sstatus(sstatus);
    drop(intr);
}

fn device_interrupt(scause: u64) -> InterruptKind {
    // top bit => supervisor interrupt via PLIC
    // cause == 9 => external interrut
    if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 9 {
        // claim this interrupt
        let irq = PLIC.hart_sclaim();

        if irq == 0x0 {
            // FIXME: I have no idea what the hell this means and why
            // it's being triggered here, but... sure it's fine
            return InterruptKind::Unknown;
        } else if let Some(handler) = global_interrupt_pool().map.read().get(&(irq as u16)) {
            if let Some((notif, badge)) = &*handler.notification.lock() {
                let mut wq = notif.wait_queue.lock();
                let word = &notif.word;
                word.fetch_or(*badge, Ordering::Relaxed);
                //    log::trace!("signal word={word:x?}");
                //log::trace!("wq={wq:?}");
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
            }
            //panic!("device_interrupt: unexpected IRQ {}", irq);
        }

        if irq != 0 {
            PLIC.hart_sunclaim(irq);
        }

        InterruptKind::External
    } else if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 5 {
        asm::timer_intr_clear();
        asm::timer_intr_off();

        let interval = LOCAL_HART.timer_interval.get();
        sbi::timer::set_timer(asm::read_time() + interval).unwrap();
        asm::timer_intr_on();

        InterruptKind::Timer
    } else if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 1 {
        asm::software_intr_clear();
        asm::software_intr_off();
        asm::software_intr_on();
        InterruptKind::Software
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
    ld tp,   24(sp)
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
    LOCAL_HART.inhibit_intena.replace(true);
    let mut intr = InterruptDisabler::new();
    // Make sure we interrupt into kernel_trapvec, now that we're in S-mode.
    // SAFETY: The address we provide is valid.
    unsafe { asm::write_stvec(kernel_trapvec as *const u8) }

    debug_assert!(
        asm::read_sstatus() & SSTATUS_SPP == 0,
        "user_trap: Did not trap from U-mode, sstatus={}",
        asm::read_sstatus()
    );

    {
        let mut thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();

        // We cannot deadlock here. Full stop. Here's why:
        // 1. We are the user trap handler. If this lock was held by
        // the same thread, then something has gone wrong--we can't
        // access this datastructure from U-mode.
        // 2. Even if there were any instances of this lock held
        // before, we wouldn't have been able to interrupt unless
        // someone enabled interrupts while the lock was still held
        // (which is a bug itself).
        let mut trapframe_lock = thread.trapframe.lock();
        trapframe_lock.user_epc = asm::read_sepc();

        let scause = asm::read_scause();
        // Exception, not a syscall or interrupt.
        assert!(
            scause & SCAUSE_INTR_BIT != 0 || scause == 8,
            "user exception: {}, hart={} pid={} sepc={:#x} stval={:#x} scause={:#x}",
            describe_exception(scause),
            hartid(),
            thread.tid,
            asm::read_sepc(),
            asm::read_stval(),
            scause
        );

        if scause == 8 {
            // syscall

            // `ecall` instrs are 4 bytes, skip to the instruction after
            trapframe_lock.user_epc += 4;

            let syscall_no = trapframe_lock.a0.into();
            drop(trapframe_lock);
            match syscall_no {
                SyscallNumber::DebugCapSlot => intr = sys_debug_cap_slot(thread, intr),
                SyscallNumber::DebugDumpRoot => intr = sys_debug_dump_root(thread, intr),
                SyscallNumber::DebugPrint => intr = sys_debug_print(thread, intr),
                SyscallNumber::CaptrDelete => intr = sys_delete(thread, intr),
                SyscallNumber::PageMap => intr = sys_page_map(thread, intr),
                SyscallNumber::DebugCapIdentify => {
                    intr = sys_debug_cap_identify(thread, intr);
                }
                SyscallNumber::ThreadSuspend => intr = sys_thread_suspend(thread, intr),
                SyscallNumber::ThreadConfigure => intr = sys_thread_configure(thread, intr),
                SyscallNumber::ThreadResume => intr = sys_thread_resume(thread, intr),
                SyscallNumber::ThreadWriteRegisters => {
                    intr = sys_thread_write_registers(thread, intr);
                }
                SyscallNumber::CaptrGrant => intr = sys_grant(thread, intr),
                SyscallNumber::Yield => intr = sys_yield(thread, intr),
                SyscallNumber::NotificationCreate => intr = sys_notification_create(thread, intr),
                SyscallNumber::NotificationWait => intr = sys_notification_wait(thread, intr),
                SyscallNumber::NotificationSignal => intr = sys_notification_signal(thread, intr),
                SyscallNumber::NotificationPoll => intr = sys_notification_poll(thread, intr),
                SyscallNumber::IntrPoolGet => intr = sys_intr_pool_get(thread, intr),
                SyscallNumber::IntrHandlerBind => intr = sys_intr_handler_bind(thread, intr),
                SyscallNumber::IntrHandlerAck => intr = sys_intr_handler_ack(thread, intr),
                SyscallNumber::IntrHandlerUnbind => intr = sys_intr_handler_unbind(thread, intr),
                SyscallNumber::EndpointRecv => intr = sys_endpoint_recv(thread, intr),
                SyscallNumber::EndpointSend => intr = sys_endpoint_send(thread, intr),
                SyscallNumber::EndpointReply => intr = sys_endpoint_reply(thread, intr),
                SyscallNumber::EndpointCall => intr = sys_endpoint_call(thread, intr),
                // TODO: MCP
                SyscallNumber::ThreadSetPriority => intr = sys_thread_set_priority(thread, intr),
                SyscallNumber::ThreadSetIpcBuffer => intr = sys_thread_set_ipc_buffer(thread, intr),
                SyscallNumber::JobCreate => intr = sys_job_create(thread, intr),
                SyscallNumber::JobCreateThread => intr = sys_job_create_thread(thread, intr),
                SyscallNumber::PageCreate => intr = sys_page_create(thread, intr),
                SyscallNumber::PageTableCreate => intr = sys_pgtbl_create(thread, intr),
                SyscallNumber::ThreadStart => intr = sys_thread_start(thread, intr),
                SyscallNumber::EndpointCreate => intr = sys_endpoint_create(thread, intr),
                _ => {
                    thread.trapframe.lock().a0 = CapError::InvalidOperation.into();
                }
            }

            thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();

            let mut private = thread.private.lock();
            if private.state == ThreadState::Suspended {
                drop(private);
                // SAFETY: This thread is currently running on this hart.
                unsafe {
                    thread.yield_to_scheduler_final();
                }
            } else if private.state == ThreadState::Blocking {
                // Make sure we don't hold any locks while we context
                // switch.

                private.state = ThreadState::Runnable;
                drop(private);
                drop(thread);
                // SAFETY: This thread is currently running on this
                // hart, or is in the wait queue.
                unsafe {
                    Thread::yield_to_scheduler();
                }
            }
        } else {
            let kind = device_interrupt(scause);
            assert!(
                kind != InterruptKind::Unknown,
                "user trap from unknown device, pid={} sepc={:#x} stval={:#x} scause={:#x}",
                thread.tid,
                trapframe_lock.user_epc,
                asm::read_stval(),
                scause
            );
            let mut private = thread.private.lock();
            let state = private.state;
            if state == ThreadState::Suspended {
                drop(trapframe_lock);
                drop(private);
                drop(intr);
                // SAFETY: This thread is currently running on this hart.
                unsafe {
                    thread.yield_to_scheduler_final();
                }
                intr = InterruptDisabler::new();
            } else if state == ThreadState::Blocking
                || kind == InterruptKind::Timer
                || kind == InterruptKind::Software
            {
                // Make sure we don't hold any locks while we context
                // switch.
                drop(trapframe_lock);

                if private.prio.effective() < 0 {
                    //                    crate::info!("hartid={} kind={:?}", hartid(), kind);
                    private.state = ThreadState::Runnable;
                }
                // Never reschedule the idle process.
                else if private.prio.effective() >= 0 && state != ThreadState::Blocking {
                    //log::trace!("timer, blocking, or IPI, thread={proc:?}, private={private:?}");

                    if kind == InterruptKind::Timer {
                        //Scheduler::debug();
                        //crate::println!("=====^before^=====");

                        let old_prio = private.prio.effective();
                        Scheduler::dequeue_dl(&thread, &mut private);
                        thread.prio_diminish_dl(&mut private);
                        if private.prio.effective() > old_prio {
                            Scheduler::enqueue_front_dl(&thread, Some(hartid()), &mut private);
                        } else {
                            Scheduler::enqueue_dl(&thread, Some(hartid()), &mut private);
                        }
                        //Scheduler::debug();
                        //crate::println!("=====^after^=====");
                        private.state = ThreadState::Runnable;
                    } else if kind == InterruptKind::Software
                        && private.state == ThreadState::Running
                    {
                        private.state = ThreadState::Runnable;
                    }
                }
                drop(private);
                drop(thread);
                drop(intr);
                // SAFETY: This thread is currently running on this
                // hart, or is in the wait queue.
                unsafe {
                    Thread::yield_to_scheduler();
                }
                intr = InterruptDisabler::new();
            }
        }
    } // guards: trapframe

    drop(intr);
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
    let intr = InterruptDisabler::new();
    // SAFETY: The address we provide is valid.
    unsafe { asm::write_stvec(trampoline() as *const u8) };

    let satp = {
        let proc = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();

        let private = proc.private.lock();

        // SAFETY: Our caller guarantees that we are within a valid
        // process's context.

        let mut trapframe = proc.trapframe.lock();
        trapframe.kernel_satp = asm::get_satp().as_usize() as u64;
        trapframe.kernel_sp =
            proc.stack.as_phys().into_virt().into_usize() as u64 + THREAD_STACK_SIZE as u64;
        trapframe.kernel_trap = user_trap as usize as u64;
        trapframe.kernel_tp = asm::tp();

        asm::write_sepc(trapframe.user_epc);
        asm::write_sscratch(private.trapframe_addr as u64);
        let pgtbl = private.root_pgtbl.as_ref().unwrap();
        let asid = pgtbl.asid();
        CURRENT_ASID[hartid() as usize].store(asid, Ordering::Relaxed);
        let satp = Satp {
            asid,
            ppn: pgtbl.as_physical_const().ppn(),
        };
        satp.encode().as_usize()
    };

    // Enable interrupts in user mode
    asm::set_spie();
    // Clear SPP = user mode
    asm::clear_spp();

    // SAFETY: The function pointer type and address are valid.
    let ret_user = unsafe {
        mem::transmute::<usize, unsafe extern "C" fn(usize) -> !>(trampoline::ret_user())
    };

    drop(intr);
    LOCAL_HART.inhibit_intena.set(false);
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

impl IntoTrapframe for MessageHeader {
    fn into_trapframe(self) -> (u64, u64) {
        (self.into_raw(), 0)
    }
}

macro_rules! define_syscall {
    ($ident:ident, $arity:tt) => {
	fn $ident(thread: ::alloc::sync::Arc<$crate::capability::Thread>, intr: InterruptDisabler) -> InterruptDisabler {
	    let trapframe_lock = thread.trapframe.lock();
	    define_syscall!(@arity thread, trapframe_lock, $ident, intr, $arity)
	}
    };

    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 7) => {{
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
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	    arg7,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 6) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	    arg6,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr

    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 5) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	    arg5,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 4) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	    arg3,
	    arg4,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 3) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	    arg3,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	    arg3,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 2) => {{
	use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	    arg2,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	    arg2,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 1) => {{
	  use $crate::trap::{FromTrapframe, IntoTrapframe};
	let (
	    arg1,
	) = FromTrapframe::from_trapframe(&$trapframe);
	drop($trapframe);
	let (a0, a1) = syscalls::$ident(
	    $thread,
	    $intr,
	    arg1,
	).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
    (@arity $thread:ident, $trapframe:ident, $ident:ident, $intr:ident, 0) => {{
	use $crate::trap::IntoTrapframe;
	drop($trapframe);
	let (a0, a1) = syscalls::$ident($thread, $intr).into_trapframe();
	let intr = InterruptDisabler::new();

	let thread = LOCAL_HART.thread.borrow().as_ref().cloned().unwrap();
	let mut trapframe_lock = thread.trapframe.lock();
	let trapframe =  &mut **trapframe_lock;

	trapframe.a0 = a0;
	trapframe.a1 = a1;
	intr
    }};
}

define_syscall!(sys_debug_dump_root, 0);
define_syscall!(sys_debug_cap_slot, 1);
define_syscall!(sys_debug_print, 2);
define_syscall!(sys_delete, 1);
define_syscall!(sys_page_map, 4);
define_syscall!(sys_debug_cap_identify, 1);
define_syscall!(sys_thread_suspend, 1);
define_syscall!(sys_thread_configure, 3);
define_syscall!(sys_thread_resume, 1);
define_syscall!(sys_thread_write_registers, 2);
define_syscall!(sys_grant, 3);
define_syscall!(sys_notification_create, 0);
define_syscall!(sys_notification_signal, 1);
define_syscall!(sys_notification_poll, 1);
define_syscall!(sys_intr_pool_get, 2);
define_syscall!(sys_intr_handler_ack, 1);
define_syscall!(sys_intr_handler_bind, 2);
define_syscall!(sys_intr_handler_unbind, 1);
define_syscall!(sys_thread_set_priority, 2);
define_syscall!(sys_thread_set_ipc_buffer, 2);
define_syscall!(sys_yield, 0);
define_syscall!(sys_notification_wait, 1);
define_syscall!(sys_endpoint_recv, 3);
define_syscall!(sys_endpoint_send, 3);
define_syscall!(sys_endpoint_reply, 3);
define_syscall!(sys_endpoint_call, 3);
define_syscall!(sys_job_create, 1);
define_syscall!(sys_job_create_thread, 3);
define_syscall!(sys_page_create, 1);
define_syscall!(sys_pgtbl_create, 0);
define_syscall!(sys_thread_start, 5);
define_syscall!(sys_endpoint_create, 0);
