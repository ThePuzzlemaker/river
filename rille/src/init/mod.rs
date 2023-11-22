// TODO
#![allow(missing_docs)]
//! This module provides some structures and utilities for
//! initialization. Some of this module is opinionated; the rest is
//! simply the information the kernel provides to the init process.

use crate::{
    addr::{Identity, PhysicalMut},
    capability::{
        paging::{BasePage, Page, PageCaptr, PageTable},
        Captbl,
        Captr,
        CaptrRange, //Untyped,
    },
};

#[derive(Debug)]
#[repr(C)]
pub struct BootInfo {
    pub captbl_size_log2: u8,
    /// Pages of the initial userspace process memory, ordered by
    /// virtual address.
    pub init_pages: CaptrRange<Page<BasePage>>,
    // pub untyped_caps: CaptrRange<Untyped>,
    // untyped_desc: [UntypedDescription; 0],
}

#[repr(C, align(8))]
#[derive(Debug)]
pub struct UntypedDescription {
    pub phys_addr: PhysicalMut<u8, Identity>,
    pub size_log2: u8,
}

pub struct InitCapabilities {
    pub captbl: Captr<Captbl>,
    pub pgtbl: Captr<PageTable>,
    pub bootinfo_page: PageCaptr<BasePage>,
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
            bootinfo_page: Captr::from_raw_unchecked(3),
        }
    }
}
