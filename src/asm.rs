use core::arch::asm;

use crate::{
    addr::{DirectMapped, VirtualMut},
    hart_local::{self, LOCAL_HART},
    paging::RawSatp,
};

// TODO: proper bitflags-type thing
/// Supervisor Interrupt Enable
pub const SSTATUS_SIE: u64 = 1 << 1;
pub const SSTATUS_SPP: u64 = 1 << 8;

#[inline]
pub fn read_sstatus() -> u64 {
    let mut x: u64;
    // SAFETY: We're just reading some data.
    unsafe { asm!("csrr {}, sstatus", out(reg) x, options(nostack)) }
    x
}

#[inline]
pub fn write_sstatus(x: u64) {
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrw sstatus, {}", in(reg) x, options(nostack)) }
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
    unsafe { asm!("csrrc {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack)) }
    r != 0
}

/// Enable S-interrupts. Returns whether or not they were enabled before.
#[inline]
pub fn intr_on() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack)) }
    r != 0
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
    unsafe { asm!("mv tp, {}", in(reg) v.into_usize(), options(nostack)) };
    tp
}

// todo: move this elsewhere
pub fn hartid() -> u64 {
    let tp = tp() as usize;
    // Before TLS is enabled, tp contains the hartid.
    if hart_local::enabled() {
        LOCAL_HART.with(|hart| hart.hartid.get())
    } else {
        tp as u64
    }
}

#[inline]
pub fn get_satp() -> RawSatp {
    let satp: u64;
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nostack)) }
    RawSatp::new_unchecked(satp)
}

#[inline]
pub fn software_intr_on() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_SSIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn software_intr_off() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_SSIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn external_intr_on() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_SEIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn external_intr_off() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_SEIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn timer_intr_on() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_STIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn timer_intr_off() -> bool {
    let r: u8;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_STIE, options(nostack)) }
    r != 0
}

#[inline]
pub fn timer_intr_clear() {
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) _, in(reg) SIP_STIP, options(nostack)) }
}

const SIE_SSIE: usize = 1 << 1;
const SIE_STIE: usize = 1 << 5;
const SIP_STIP: usize = 1 << 5;
const SIE_SEIE: usize = 1 << 9;

#[inline]
pub fn read_sepc() -> u64 {
    let r: u64;
    unsafe { asm!("csrr {}, sepc", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn read_scause() -> u64 {
    let r: u64;
    unsafe { asm!("csrr {}, scause", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn write_sepc(sepc: u64) {
    unsafe { asm!("csrw sepc, {}", in(reg) sepc, options(nostack)) }
}

#[inline]
pub fn read_stval() -> u64 {
    let r: u64;
    unsafe { asm!("csrr {}, stval", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn write_stval(stval: u64) {
    unsafe { asm!("csrw stval, {}", in(reg) stval, options(nostack)) }
}

pub const SCAUSE_INTR_BIT: u64 = 1 << 63;

#[inline]
pub fn read_time() -> u64 {
    let r: u64;
    unsafe { asm!("csrr {}, time", out(reg) r, options(nostack)) }
    r
}
