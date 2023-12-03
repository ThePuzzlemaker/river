//! This module provides some structures and utilities for
//! initialization. Some of this module is opinionated; the rest is
//! simply the information the kernel provides to the init process.

use crate::capability::{
    paging::{BasePage, Page, PageCaptr, PageTable},
    Allocator, //Untyped,
    Captbl,
    Captr,
    CaptrRange,
    Empty,
    InterruptPool,
    Thread,
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
    /// The initial process's captbl.
    pub captbl: Captr<Captbl>,
    /// The initial process's page table.
    pub pgtbl: Captr<PageTable>,
    /// The initial process's thread capability.
    pub thread: Captr<Thread>,
    /// The kernel allocator.
    pub allocator: Captr<Allocator>,
    /// The page backing the [`BootInfo`] passed to the init proces.
    pub bootinfo_page: PageCaptr<BasePage>,
    /// The global interrupt pool.
    pub intr_pool: Captr<InterruptPool>,
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
            captbl: Captr::from_raw_unchecked(1),
            pgtbl: Captr::from_raw_unchecked(2),
            thread: Captr::from_raw_unchecked(3),
            allocator: Captr::from_raw_unchecked(4),
            bootinfo_page: Captr::from_raw_unchecked(5),
            intr_pool: Captr::from_raw_unchecked(6),
        }
    }
}
