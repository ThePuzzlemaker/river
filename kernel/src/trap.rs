use core::{arch::global_asm, mem, slice};

use crate::{
    asm::{self, hartid, intr_enabled, SCAUSE_INTR_BIT, SSTATUS_SPP},
    capability::{captbl::Captbl, AnyCap, EmptySlot},
    hart_local::LOCAL_HART,
    paging::Satp,
    plic::PLIC,
    print, println,
    proc::ProcState,
    sync::{OnceCell, SpinRwLockReadGuard},
    trampoline::{self, trampoline},
    uart,
};
use rille::{
    addr::{Virtual, VirtualConst},
    capability::CapabilityType,
    syscalls::SyscallNumber,
    units::StorageUnits,
};

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
    LOCAL_HART.with(|hart| {
        if kind == InterruptKind::Timer {
            if let Some(token) = hart.proc() {
                let proc = token.proc();

                if proc.state() == ProcState::Running {
                    proc.set_state(ProcState::Runnable);
                    token.yield_to_scheduler();
                }
            }
        }
    });

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

    LOCAL_HART.with(|hart| {
        hart.trap.set(true);

        let token = hart.proc().unwrap();
        let proc = token.proc();

        let mut private = proc.private(&token);

        // SAFETY: Being in this process's context means that the
        // trapframe must be initialized and valid.
        let trapframe = unsafe { (*private.trapframe).assume_init_mut() };
        trapframe.user_epc = asm::read_sepc();

        let scause = asm::read_scause();
        // Exception, not a syscall or interrupt.
        assert!(
            scause & SCAUSE_INTR_BIT != 0 || scause == 8,
            "user exception: {}, hart={} pid={} sepc={:#x} stval={:#x} scause={:#x}",
            describe_exception(scause),
            hartid(),
            proc.pid,
            asm::read_sepc(),
            asm::read_stval(),
            scause
        );

        if scause == 8 {
            // syscall

            // `ecall` instrs are 4 bytes, skip to the instruction after
            trapframe.user_epc += 4;

            match trapframe.a0.into() {
                SyscallNumber::DebugPrint => {
                    let str_ptr = trapframe.a1 as usize;
                    let str_len = trapframe.a2 as usize;

                    assert!(str_ptr + str_len < private.mem_size, "too long");

                    // SAFETY: All values are valid for u8 and process
                    // memory is zeroed and is thus never
                    // uninitialized.
                    let slice = unsafe {
                        slice::from_raw_parts(
                            private
                                .mman
                                .get_table()
                                .walk(Virtual::from(str_ptr))
                                .unwrap()
                                .0
                                .into_virt()
                                .into_ptr(),
                            str_len,
                        )
                    };
                    let s = core::str::from_utf8(slice).unwrap();
                    print!("{}", s);

                    trapframe.a0 = 0;
                }
                SyscallNumber::CopyDeep => {
                    let from_tbl_ref = trapframe.a1 as usize;
                    let from_tbl_index = trapframe.a2 as usize;
                    let from_index = trapframe.a3 as usize;
                    let into_tbl_ref = trapframe.a4 as usize;
                    let into_tbl_index = trapframe.a5 as usize;
                    let into_index = trapframe.a6 as usize;

                    let root_hdr = &private.captbl;

                    let from_tbl_index: Captbl = {
                        let from_tbl_ref = if from_tbl_ref == 0 {
                            root_hdr.clone()
                        } else {
                            let root_lock = root_hdr.read();
                            root_lock
                                .get::<Captbl>(from_tbl_ref)
                                .map(Captbl::from)
                                .unwrap()
                        };
                        let from_tbl_ref = from_tbl_ref.read();

                        from_tbl_ref.get::<Captbl>(from_tbl_index).unwrap().into()
                    };

                    let from_tbl_index_lock = from_tbl_index.read();

                    let into_tbl_index = {
                        let into_tbl_ref = if into_tbl_ref == 0 {
                            root_hdr.clone()
                        } else {
                            let root_lock = root_hdr.read();
                            root_lock
                                .get::<Captbl>(into_tbl_ref)
                                .map(Captbl::from)
                                .unwrap()
                        };
                        let into_tbl_ref = into_tbl_ref.read();

                        into_tbl_ref.get::<Captbl>(into_tbl_index).unwrap().clone()
                    };

                    if from_tbl_index == into_tbl_index {
                        let mut into_tbl_index = from_tbl_index_lock.upgrade();

                        let (mut into_index, mut from_index) = into_tbl_index
                            .get2_mut::<EmptySlot, AnyCap>(into_index, from_index)
                            .unwrap();

                        let mut into_index = into_index.replace(AnyCap::from(&mut from_index));
                        from_index.add_child(&mut into_index);
                    } else {
                        let mut from_tbl_index = from_tbl_index_lock.upgrade();
                        let mut into_tbl_index = into_tbl_index.write();

                        let mut into_index =
                            into_tbl_index.get_mut::<EmptySlot>(into_index).unwrap();
                        let mut from_index = from_tbl_index.get_mut::<AnyCap>(from_index).unwrap();

                        let mut into_index = into_index.replace(AnyCap::from(&mut from_index));
                        from_index.add_child(&mut into_index);
                    }

                    trapframe.a0 = 0;
                }
                SyscallNumber::DebugCapSlot => {
                    let tbl = trapframe.a1 as usize;
                    let index = trapframe.a2 as usize;
                    // SAFETY: By invariants.
                    let root_hdr = &private.captbl;
                    let root_lock = root_hdr.read();
                    let tbl = if tbl == 0 {
                        root_hdr.clone()
                    } else {
                        root_lock.get(tbl).unwrap().clone()
                    };
                    let tbl = tbl.read();

                    let slot = tbl.get::<AnyCap>(index).unwrap();
                    println!("{:#?}", slot);

                    trapframe.a0 = 0;
                }
                SyscallNumber::DebugDumpRoot => {
                    // SAFETY: By invariants.
                    println!("{:#x?}", private.captbl);

                    // let inner = private.captbl_ptr.as_virt_addr().cast::<u8>();
                    // println!("size: {:?}", private.captbl_ptr.size_bytes());
                    // let slice = unsafe {
                    //     core::slice::from_raw_parts(
                    //         inner.into_ptr(),
                    //         private.captbl_ptr.size_bytes(),
                    //     )
                    // };
                    // //println!("{:?}", slice);

                    // println!(
                    //     "run in qemu console: dump-guest-memory captbl.bin {:#x} {}",
                    //     inner.into_phys().into_usize(),
                    //     slice.len()
                    // );
                    trapframe.a0 = 0;
                }
                // dump-guest-memory captbl.bin 0x84402000
                _ => todo!("invalid syscall number"),
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
                proc.pid,
                trapframe.user_epc,
                asm::read_stval(),
                scause
            );
            if kind == InterruptKind::Timer {
                drop(private);
                token.proc().set_state(ProcState::Runnable);
                token.yield_to_scheduler();
            }
        }
    });

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
pub unsafe extern "C" fn user_trap_ret() -> ! {
    asm::intr_off();
    LOCAL_HART.with(|hart| hart.trap.set(true));

    // SAFETY: The address we provide is valid.
    unsafe { asm::write_stvec(trampoline() as *const u8) };

    let satp = LOCAL_HART.with(|hart| {
        let token = hart.proc().unwrap();

        let mut private = token.proc().private(&token);

        // SAFETY: Our caller guarantees that we are within a valid
        // process's context.
        let trapframe = unsafe { (*private.trapframe).assume_init_mut() };
        trapframe.kernel_satp = asm::get_satp().as_usize() as u64;
        trapframe.kernel_sp = (private.kernel_stack.into_usize() as u64) + 4u64.mib();
        trapframe.kernel_trap = user_trap as u64;
        trapframe.kernel_tp = asm::tp();

        asm::write_sepc(trapframe.user_epc);
        let satp = Satp {
            asid: 1,
            ppn: private.mman.get_table().as_physical_const().ppn(),
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
