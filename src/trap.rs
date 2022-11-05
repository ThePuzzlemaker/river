use core::arch::global_asm;

use crate::{
    asm::{self, intr_enabled, SCAUSE_INTR_BIT, SSTATUS_SPP},
    once_cell::OnceCell,
    plic::PLIC,
    spin::SpinMutex,
    uart,
};

pub struct Irqs {
    pub uart: u32,
}

pub static IRQS: OnceCell<Irqs> = OnceCell::new();

#[no_mangle]
pub unsafe extern "C" fn kernel_trap() {
    let sepc = asm::read_sepc();
    let sstatus = asm::read_sstatus();
    let scause = asm::read_scause();

    debug_assert_ne!(
        sstatus & SSTATUS_SPP,
        0,
        "kernel_trap: did not trap from S-mode"
    );
    debug_assert!(!intr_enabled(), "kernel_trap: interrupts enabled");

    // Not an interrupt.
    if scause & SCAUSE_INTR_BIT == 0 {
        panic!(
            "kernel exception: {}, sepc={:#x} stval={:#x} scause={:#x}",
            describe_exception(scause),
            sepc,
            asm::read_stval(),
            scause
        )
    }

    let device = device_interrupt(scause);
    if device == 0 {
        panic!(
            "kernel trap from unknown device, sepc={:#x} stval={:#x} scause={:#x}",
            sepc,
            asm::read_stval(),
            scause
        )
    }

    asm::write_sepc(sepc);
    asm::write_sstatus(sstatus);
}

fn device_interrupt(scause: u64) -> u64 {
    // top bit => supervisor interrupt via PLIC
    // cause == 9 => external interrut
    if scause & SCAUSE_INTR_BIT != 0 && scause & 0xff == 9 {
        // claim this interrupt
        let irq = unsafe { PLIC.hart_sclaim() };

        if irq == IRQS.expect("IRQS").uart {
            uart::handle_interrupt();
        } else {
            panic!("device_interrupt: unexpected IRQ {}", irq);
        }

        1
    } else {
        0
    }
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
    # not tp (contains hart-local data), in case we moved CPUs
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
