use core::arch::asm;

use crate::paging::RawSatp;

// TODO: proper bitflags-type thing
/// Supervisor Interrupt Enable
pub const SSTATUS_SIE: u64 = 1 << 1;

#[inline]
pub fn read_sstatus() -> u64 {
    let mut x: u64;
    // SAFETY: We're just reading some data.
    unsafe {
        asm!("csrr {}, sstatus", out(reg) x, options(nostack));
    }
    x
}

#[inline]
pub fn write_sstatus(x: u64) {
    // SAFETY: Writes to CSRs are atomic.
    unsafe {
        asm!("csrw sstatus, {}", in(reg) x, options(nostack));
    }
}

#[inline]
pub fn intr_enabled() -> bool {
    (read_sstatus() & SSTATUS_SIE) != 0
}

/// Disable S-interrupts. Returns whether or not they were enabled before.
#[inline]
pub fn intr_off() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe {
        asm!("csrrc {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack));
    }
    r != 0
}

/// Enable S-interrupts. Returns whether or not they were enabled before.
#[inline]
pub fn intr_on() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe {
        asm!("csrrs {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack));
    }
    r != 0
}

pub fn with_interrupts<T>(f: impl FnOnce() -> T) -> T {
    intr_on();
    let r = f();
    intr_off();
    r
}

pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let prev = intr_off();
    let r = f();
    if prev {
        intr_on();
    }
    r
}

#[inline]
pub fn hartid() -> u64 {
    let x: u64;
    // SAFETY: We are just reading some data to a known valid variable.
    unsafe { asm!("mv {}, tp", out(reg) x, options(nostack)) }
    x
}

#[inline]
pub fn get_satp() -> RawSatp {
    let satp: u64;
    unsafe { asm!("csrr {}, satp", out(reg) satp) };
    RawSatp::new_unchecked(satp)
}
