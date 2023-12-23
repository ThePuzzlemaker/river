use core::{mem::MaybeUninit, ops::Range};

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};

use rille::{
    addr::{Identity, PhysicalConst, VirtualConst},
    capability::paging::PageSize,
    units::StorageUnits,
};

use crate::{
    kalloc::{self, phys::PMAlloc},
    paging::{PageTableFlags, PagingAllocator, SharedPageTable},
    sync::SpinRwLock,
};

#[derive(Debug)]
pub struct UserMemoryManager {
    table: SharedPageTable,
    map: BTreeMap<VirtualConst<u8, Identity>, VmemRegion>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VmemRegion {
    Mapped {
        region: MemRegion,
        span: Range<VirtualConst<u8, Identity>>,
        flags: PageTableFlags,
    },
    Unoccupied,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MemRegion {
    Guard,
    Backed(PmemRegion),
}

#[derive(Debug, PartialEq, Eq)]
pub enum PmemRegion {
    Contiguous(PhysicalConst<u8, Identity>),
    Sparse(Vec<PhysicalConst<u8, Identity>>),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RegionRequest {
    pub n_pages: usize,
    pub contiguous: bool,
    pub flags: PageTableFlags,
    pub purpose: RegionPurpose,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RegionPurpose {
    UserMapped,
    Guard,
    Trapframe,
    Trampoline,
    Stack,
    Unknown,
}

impl Default for UserMemoryManager {
    fn default() -> Self {
        // SAFETY: The page is zeroed and thus is well-defined and valid.
        let table =
            unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) };
        Self {
            table: SharedPageTable::from_inner(table),
            map: BTreeMap::new(),
        }
    }
}

impl UserMemoryManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    #[track_caller]
    pub fn alloc(
        &mut self,
        req: &RegionRequest,
        addr: Option<VirtualConst<u8, Identity>>,
    ) -> Result<Range<VirtualConst<u8, Identity>>, AllocError> {
        // TODO: find a good address
        let addr = addr.unwrap();

        let backing_mem = if req.contiguous {
            let mut pma = PMAlloc::get();
            pma.allocate(kalloc::phys::what_order(req.n_pages * 4.kib()))
                .ok_or(AllocError)?
        } else {
            todo!("sparse");
        };

        for page in 0..req.n_pages {
            let from = backing_mem.add(page * 4.kib()).into_identity().into_const();
            let to = addr.add(page * 4.kib());
            let flags = req.flags;
            self.table.map(None, from, to, flags, PageSize::Base);
        }

        let range = addr..addr.add(req.n_pages * 4.kib());
        // TODO: proper range arithmetic
        self.map.insert(
            range.end,
            VmemRegion::Mapped {
                region: MemRegion::Backed(PmemRegion::Contiguous(
                    backing_mem.into_identity().into_const(),
                )),
                span: range.clone(),
                flags: req.flags,
            },
        );

        Ok(range)
    }

    pub fn map_direct(
        &mut self,
        from: PhysicalConst<u8, Identity>,
        to: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) {
        self.table.map(None, from, to, flags, PageSize::Base);
    }

    pub fn get_table(&self) -> &SharedPageTable {
        &self.table
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AllocError;
