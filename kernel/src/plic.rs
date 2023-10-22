use core::ptr;

use crate::{asm, sync::spin::SpinMutex};

pub struct Plic {
    inner: SpinMutex<PlicInner>,
}

// SAFETY: The `PlicInner` is protected by a `SpinMutex`
unsafe impl Send for Plic {}
// SAFETY: See above.
unsafe impl Sync for Plic {}

struct PlicInner {
    plic_base: *mut u8,
    init: bool,
}

impl Plic {
    /// Initialize the PLIC.
    ///
    /// # Safety
    ///
    /// The `plic_base` pointer must be valid and must be of the required size
    /// of a PLIC
    pub unsafe fn init(&self, plic_base: *mut u8) {
        debug_assert_eq!(
            plic_base as usize % 4096,
            0,
            "Plic::init: literally gonna cry rn, plic_base is not page-aligned"
        );
        let mut inner = self.inner.lock();
        debug_assert!(
            !inner.init,
            "Plic::init: attempted to initialize an already initialized PLIC"
        );
        inner.plic_base = plic_base;
        inner.init = true;
    }

    /// Set the priority of an interrupt for the current hart.
    pub fn hart_set_spriority(&self, spriority: u32) {
        let hart = asm::hartid();
        let inner = self.inner.lock();
        // SAFETY: By our invariants, this is safe.
        let spriority_addr = unsafe {
            inner
                .plic_base
                .add(0x0020_1000 + 0x2000 * hart as usize)
                .cast::<u32>()
        };
        // SAFETY: By our invariants, this is safe.
        unsafe { spriority_addr.write_volatile(spriority) }
    }

    /// Enable an interrupt for the current hart.
    ///
    /// # Panics
    ///
    /// This function will panic if the `interrupt_id` is invalid (i.e., greater
    /// than 1023).
    pub fn hart_senable(&self, interrupt_id: u32) {
        assert!(
            interrupt_id <= 1023,
            "Plic::hart_senable: cannot enable an interrupt with an ID greater than 1023"
        );
        let hart = asm::hartid();
        let offset = interrupt_id / 32;
        let bit_idx = interrupt_id % 32;
        let inner = self.inner.lock();
        // SAFETY: By our invariants, this is safe.
        let addr = unsafe {
            inner
                .plic_base
                .add(0x2080 + 0x100 * hart as usize + 4 * offset as usize)
                .cast::<u32>()
        };
        // SAFETY: By our invariants, this is safe.
        let val = unsafe { addr.read_volatile() };
        // SAFETY: By our invariants, this is safe.
        unsafe { addr.write_volatile(val | (1 << bit_idx)) };
    }

    /// Disable an interrupt for the current hart.
    ///
    /// # Panics
    ///
    /// This function will panic if the `interrupt_id` is invalid (i.e., greater
    /// than 1023).
    pub fn hart_sdisable(&self, interrupt_id: u32) {
        assert!(
            interrupt_id <= 1023,
            "Plic::hart_sdisable: cannot disable an interrupt with an ID greater than 1023"
        );
        let hart = asm::hartid();
        let offset = interrupt_id / 32;
        let bit_idx = interrupt_id % 32;
        let inner = self.inner.lock();
        // SAFETY: By our invariants, this is safe.
        let addr = unsafe {
            inner
                .plic_base
                .add(0x2080 + 0x100 * hart as usize + offset as usize)
                .cast::<u32>()
        };
        // SAFETY: By our invariants, this is safe.
        let val = unsafe { addr.read_volatile() };
        // SAFETY: By our invariants, this is safe.
        unsafe { addr.write_volatile(val & !(1 << bit_idx)) };
    }

    /// Set the priority of an interrupt for the current hart
    ///
    /// # Panics
    ///
    /// This function will panic if the `interrupt_id` is invalid (i.e., greater
    /// than 1023).
    pub fn set_priority(&self, interrupt_id: u32, priority: u32) {
        assert!(interrupt_id <= 1023, "Plic::set_priority: cannot set the priority of an interrupt with an ID greater than 1023");
        let inner = self.inner.lock();
        // SAFETY: By our invariants, this is safe.
        let addr = unsafe { inner.plic_base.add(4 * interrupt_id as usize).cast::<u32>() };
        // SAFETY: By our invariants, this is safe.
        unsafe { addr.write_volatile(priority) }
    }

    /// Claim an interrupt for the current hart.
    pub fn hart_sclaim(&self) -> u32 {
        let inner = self.inner.lock();
        let hart = asm::hartid();
        // SAFETY: By our invariants, this is safe.
        let addr = unsafe {
            inner
                .plic_base
                .add(0x0020_1004 + 0x2000 * hart as usize)
                .cast::<u32>()
        };
        // SAFETY: By our invariants, this is safe.
        unsafe { addr.read_volatile() }
    }

    /// Unclaim an interrupt for the current hart.
    ///
    /// # Panics
    ///
    /// This function will panic if the `interrupt_id` is invalid (i.e., greater
    /// than 1023).
    pub fn hart_sunclaim(&self, interrupt_id: u32) {
        assert!(
            interrupt_id <= 1023,
            "Plic::hart_sunclaim: cannot unclaim an interrupt with an ID greater than 1023"
        );
        let hart = asm::hartid();
        let inner = self.inner.lock();
        // SAFETY: By our invariants, this is valid.
        let addr = unsafe {
            inner
                .plic_base
                .add(0x0020_1004 + 0x2000 * hart as usize)
                .cast::<u32>()
        };
        // SAFETY: By our invariants, this is valid.
        unsafe { addr.write_volatile(interrupt_id) }
    }
}

pub static PLIC: Plic = Plic {
    inner: SpinMutex::new(PlicInner {
        plic_base: ptr::null_mut(),
        init: false,
    }),
};
