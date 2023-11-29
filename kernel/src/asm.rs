use core::arch::asm;
use core::intrinsics::unlikely;
use core::sync::atomic::{self, Ordering};

use crate::{
    hart_local::{self, LOCAL_HART},
    paging::RawSatp,
};

// TODO: proper bitflags-type thing
/// Supervisor Interrupt Enable
pub const SSTATUS_SIE: u64 = 1 << 1;
pub const SSTATUS_SPP: u64 = 1 << 8;
pub const SSTATUS_SPIE: u64 = 1 << 5;

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
fn intr_enabled() -> bool {
    (read_sstatus() & SSTATUS_SIE) != 0
}

#[inline(always)]
pub fn set_spie() -> bool {
    let r: u64;
    // SAFETY: Write to CSRs are atomic.
    unsafe { asm!("csrrs {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SPIE, options(nostack)) }
    r & SSTATUS_SPIE != 0
}

#[inline(always)]
pub fn clear_spie() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrc {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SPIE, options(nostack)) }
    r & SSTATUS_SPIE != 0
}

#[inline(always)]
pub fn set_spp() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SPP, options(nostack)) }
    r & SSTATUS_SPP != 0
}

#[inline(always)]
pub fn clear_spp() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrc {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SPP, options(nostack)) }
    r & SSTATUS_SPP != 0
}

/// Disable S-interrupts. Returns whether or not they were enabled before.
#[inline(always)]
pub fn intr_off() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrc {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack)) }
    r & SSTATUS_SIE != 0
}

/// Enable S-interrupts. Returns whether or not they were enabled before.
#[inline(always)]
#[track_caller]
pub fn intr_on() -> bool {
    debug_assert_eq!(
        LOCAL_HART.holding_disabler.get(),
        0,
        "asm::intr_on: holding disabler"
    );
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sstatus, {}", out(reg) r, in(reg) SSTATUS_SIE, options(nostack)) }
    r & SSTATUS_SIE != 0
}

#[derive(Debug)]
#[repr(C)]
pub struct InterruptDisabler(u8);

impl InterruptDisabler {
    #[track_caller]
    #[allow(clippy::missing_panics_doc)]
    pub fn new() -> Self {
        let prev = intr_off();
        if !hart_local::enabled() {
            return Self(0);
        }

        atomic::compiler_fence(Ordering::SeqCst);

        // DEBUG: If interrupts were previously disabled, make sure
        // they didn't get enabled in the meantime.
        debug_assert!(
            !(prev && LOCAL_HART.holding_disabler.get() != 0),
            "InterruptDisabler::new: interrupts were enabled when intena was previously disabled"
        );
        LOCAL_HART.holding_disabler.update(|x| x + 1);
        if LOCAL_HART.holding_disabler.get() == 1 {
            LOCAL_HART.intena.set(prev);
        }
        Self(0)
    }
}

impl Default for InterruptDisabler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for InterruptDisabler {
    fn drop(&mut self) {
        debug_assert!(
            !intr_enabled(),
            "InterruptDisabler: interrupts were enabled"
        );
        if !hart_local::enabled() {
            // Interrupts are disabled before hart-local data is enabled.
            return;
        }

        atomic::compiler_fence(Ordering::SeqCst);
        // TODO: figure out where this is trying to overflow
        let new = LOCAL_HART.holding_disabler.update(|x| x - 1);

        if LOCAL_HART.intena.get() && new == 0 && !LOCAL_HART.inhibit_intena.get() {
            intr_on();
        }
    }
}

#[inline(always)]
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
#[inline(always)]
pub unsafe fn set_tp(v: usize) -> u64 {
    let tp = tp();
    // SAFETY: Our caller guarantees this is safe.
    unsafe { asm!("mv tp, {}", in(reg) v, options(nostack)) };
    tp
}

// todo: move this elsewhere
/// Get the hart ID of the current hart.
///
/// # Panics
///
/// This function will panic if interrupts are enabled when called.
#[track_caller]
#[inline(always)]
pub fn hartid() -> u64 {
    let tp = tp() as usize;
    // Before TLS is enabled, tp contains the hartid.
    if unlikely(!hart_local::enabled()) {
        return tp as u64;
    }

    // Prevent this load from getting moved up.
    atomic::compiler_fence(Ordering::SeqCst);

    LOCAL_HART.hartid.get()
}

#[inline]
pub fn get_satp() -> RawSatp {
    let satp: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nostack)) }
    RawSatp::new_unchecked(satp)
}

#[inline]
pub fn software_intr_on() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_SSIE, options(nostack)) }
    r & SIE_SSIE != 0
}

#[inline]
pub fn software_intr_off() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_SSIE, options(nostack)) }
    r & SIE_SSIE != 0
}

#[inline]
pub fn external_intr_on() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_SEIE, options(nostack)) }
    r & SIE_SEIE != 0
}

#[inline]
pub fn external_intr_off() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_SEIE, options(nostack)) }
    r & SIE_SEIE != 0
}

#[inline]
pub fn timer_intr_on() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrrs {}, sie, {}", out(reg) r, in(reg) SIE_STIE, options(nostack)) }
    r & SIE_STIE != 0
}

#[inline]
pub fn timer_intr_off() -> bool {
    let r: u64;
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sie, {}", out(reg) r, in(reg) SIE_STIE, options(nostack)) }
    r & SIE_STIE != 0
}

#[inline]
pub fn timer_intr_clear() {
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sip, {}", out(reg) _, in(reg) SIP_STIP, options(nostack)) }
}

#[inline]
pub fn software_intr_clear() {
    // SAFETY: Writes to CSRs are atomic
    unsafe { asm!("csrrc {}, sip, {}", out(reg) _, in(reg) SIP_SSIP, options(nostack)) }
}

const SIE_SSIE: u64 = 1 << 1;
const SIE_STIE: u64 = 1 << 5;
const SIP_STIP: u64 = 1 << 5;
const SIE_SEIE: u64 = 1 << 9;
const SIP_SSIP: u64 = 1 << 1;

#[inline]
pub fn read_sepc() -> u64 {
    let r: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, sepc", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn read_scause() -> u64 {
    let r: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, scause", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn write_sepc(sepc: u64) {
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrw sepc, {}", in(reg) sepc, options(nostack)) }
}

#[inline]
pub fn write_sscratch(sscratch: u64) {
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrw sscratch, {}", in(reg) sscratch, options(nostack)) }
}

#[inline]
pub fn read_stval() -> u64 {
    let r: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, stval", out(reg) r, options(nostack)) }
    r
}

#[inline]
pub fn write_stval(stval: u64) {
    // SAFETY: Writes to CSRs are atomic.
    unsafe { asm!("csrw stval, {}", in(reg) stval, options(nostack)) }
}

pub const SCAUSE_INTR_BIT: u64 = 1 << 63;

#[inline]
pub fn read_time() -> u64 {
    let r: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, time", out(reg) r, options(nostack)) }
    r
}

#[inline(always)]
pub fn nop() {
    // SAFETY: Executing a NOP is always safe.
    unsafe { asm!("nop") }
}

#[inline(always)]
pub fn read_stvec() -> u64 {
    let r: u64;
    // SAFETY: Reads from CSRs are atomic.
    unsafe { asm!("csrr {}, stvec", out(reg) r, options(nostack)) }
    r
}

/// Write a function or vector as the trap handler.
///
/// # Safety
///
/// The address provided must be a valid value for a trap handler.
#[inline(always)]
pub unsafe fn write_stvec(stvec: *const u8) {
    // SAFETY: Writes to CSRs are atomic, and our caller guarantees the value is valid.
    unsafe { asm!("csrw stvec, {}", in(reg) stvec, options(nostack)) }
}
