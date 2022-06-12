use core::arch::asm;

use crate::{
    addr::{self, DirectMapped, Mapping, VirtualMut},
    paging::{self, RawSatp},
    HART_ID,
};

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
pub fn tp() -> u64 {
    let x: u64;
    // SAFETY: We are just reading some data to a known valid variable.
    unsafe { asm!("mv {}, tp", out(reg) x, options(nostack)) }
    x
}

/// Set the thread pointer. Returns the old thread pointer.
///
/// # Safety
///
/// This function is only safe if the thread pointer has not been set
/// elsewhere and is being set to a valid address.
#[inline]
pub unsafe fn set_tp(v: VirtualMut<u8, DirectMapped>) -> u64 {
    let tp = tp();
    asm!("mv tp, {}", in(reg) v.into_usize());
    tp
}

// todo: move this elsewhere
pub fn hartid() -> u64 {
    let tp = tp() as usize;
    // Before TLS is enabled, tp contains the hartid.
    // After it is, it contains the pointer to the thread-local storage.
    if paging::enabled() && addr::DirectMapped::vaddr_space().contains(&tp) {
        HART_ID.with(|x| x.get())
    } else {
        tp as u64
    }
}

#[inline]
pub fn get_satp() -> RawSatp {
    let satp: u64;
    unsafe { asm!("csrr {}, satp", out(reg) satp) };
    RawSatp::new_unchecked(satp)
}
