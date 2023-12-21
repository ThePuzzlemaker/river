//! This module provides some structures and utilities for
//! initialization. Some of this module is opinionated; the rest is
//! simply the information the kernel provides to the init process.

use crate::prelude::{
    paging::{BasePage, Page, PageTable},
    *,
};

/// Information provided to the initial process.
#[derive(Debug)]
#[repr(C)]
pub struct BootInfo {
    /// The size of the initial process's captbl, in number of slots
    /// log2.
    pub captbl_size_log2: u8,
    /// Pages of the initial userspace process memory, ordered by
    /// virtual address.
    pub init_pages: CaptrRange<Page<BasePage>>,
    /// Pages of the FDT, ordered by virtual address.
    pub fdt_pages: CaptrRange<Page<BasePage>>,
    /// Pages of device memory, ordered by virtual address.
    pub dev_pages: CaptrRange<Page<BasePage>>,
    /// The first free slot.
    pub free_slots: CaptrRange<Empty>,
    /// Pointer to the FDT
    pub fdt_ptr: *const u8,
}

/// Fixed-offset capabilities provided to the initial process.
pub struct InitCapabilities {
    /// The root job.
    pub job: Job,
    /// The initial process's page table.
    pub pgtbl: PageTable,
    /// The initial process's thread capability.
    pub thread: Thread,
    /// The page backing the [`BootInfo`] passed to the init proces.
    pub bootinfo_page: Page<BasePage>,
    /// The global interrupt pool.
    pub intr_pool: InterruptPool,
}

impl InitCapabilities {
    /// Create [`Captr`]s of the initial capabilities.
    ///
    /// # Safety
    ///
    /// This function must be called from the init thread, or a thread
    /// sharing its [`Captbl`].
    pub unsafe fn new() -> Self {
        Self {
            job: Job::from_captr(Captr::from_raw(1)),
            pgtbl: PageTable::from_captr(Captr::from_raw(2)),
            thread: Thread::from_captr(Captr::from_raw(3)),
            bootinfo_page: Page::from_captr(Captr::from_raw(5)),
            intr_pool: InterruptPool::from_captr(Captr::from_raw(6)),
        }
    }
}
