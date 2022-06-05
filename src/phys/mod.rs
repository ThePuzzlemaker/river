use crate::{
    addr::{DirectMapped, Physical, PhysicalMut},
    spin::{SpinMutex, SpinMutexGuard},
    units::StorageUnits,
};

static PMALLOC: SpinMutex<PMAlloc> = SpinMutex::new(PMAlloc::new_uninit());

impl PMAlloc {
    const fn new_uninit() -> Self {
        PMAlloc {
            init: false,
            frees: Physical::null(),
            start: Physical::null(),
            unmanaged: Physical::null(),
            limit: Physical::null(),
        }
    }

    /// # Safety
    ///
    /// - `start` must be less than `end`.
    /// - `end - start` must be a multiple of 4096.
    /// - `start` and `end` must both be aligned to 4096 bytes.
    /// - `start` and `end` must both be non-null.
    pub unsafe fn init(start: PhysicalMut<u8, DirectMapped>, end: PhysicalMut<u8, DirectMapped>) {
        let mut pma = PMALLOC.lock();
        pma.init = true;
        pma.start = start;
        pma.unmanaged = start;
        pma.limit = end;
    }

    /// # Panics
    ///
    /// This function will panic if the global PMAlloc has not been initialized.
    /// See [`PMAlloc::init`].
    pub fn get() -> SpinMutexGuard<'static, PMAlloc> {
        let pma = PMALLOC.lock();
        assert!(pma.init, "PMAlloc::get: not initialized");
        pma
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PMAlloc {
    init: bool,
    frees: PhysicalMut<Node, DirectMapped>,
    start: PhysicalMut<u8, DirectMapped>,
    unmanaged: PhysicalMut<u8, DirectMapped>,
    limit: PhysicalMut<u8, DirectMapped>,
}

unsafe impl Send for PMAlloc {}
unsafe impl Sync for PMAlloc {}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C, align(4096))]
struct Node {
    next: PhysicalMut<Node, DirectMapped>,
}

impl PMAlloc {
    fn create_block(&mut self) -> Option<PhysicalPage> {
        // Ensure we have enough space left.
        if self.unmanaged >= self.limit || self.unmanaged.into_usize() >= usize::MAX - 4.kib() {
            return None;
        }

        let block = PhysicalPage::from_physical(self.unmanaged.cast());

        // Bump unmanaged area
        self.unmanaged = self.unmanaged.add(4.kib());

        Some(block)
    }
}

impl PMAlloc {
    pub fn allocate(&mut self) -> Option<PhysicalPage> {
        let pg = match self.frees.is_null() {
            // We couldn't find a suitable block, create a new one.
            true => self.create_block(),
            // Found a block, splice it out of the list.
            _ => {
                let block_phys = self.frees;
                let vaddr = self.frees.into_virt();
                // SAFETY: We cannot create a PMAlloc without giving it a valid node.
                // We've also ensured the pointer is in the correct address space.
                let block: &mut Node = unsafe { &mut *vaddr.into_ptr_mut() };
                // Make sure the PMAlloc no longer has a pointer to us.
                self.frees = block.next;
                Some(PhysicalPage::from_physical(block_phys.cast()))
            }
        }?;
        let pg_vaddr = pg.into_physical().into_virt();
        // Zero out the page.
        // SAFETY: The pointer is valid.
        unsafe {
            pg_vaddr.into_ptr_mut().write_bytes(0, 4096);
        }
        Some(pg)
    }

    /// # Safety
    ///
    /// The page provided must have been allocated by this allocator and still
    /// be valid.
    pub unsafe fn deallocate(&mut self, page: PhysicalPage) {
        let node_phys = page.into_physical().cast();
        let vaddr = node_phys.into_virt();
        // SAFETY: Node is 4096 bytes and we require the pointer is valid.
        let node: &mut Node = &mut *vaddr.into_ptr_mut();
        // Put this node at the front of the free list.
        let next = self.frees;
        node.next = next;
        // Make sure we put the node in the free list as its physical variant.
        // SAFETY: We know this pointer is non-null.
        self.frees = node_phys;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysicalPage(PhysicalMut<[u8; 4096], DirectMapped>);

impl PhysicalPage {
    pub fn from_physical(paddr: PhysicalMut<[u8; 4096], DirectMapped>) -> Self {
        assert!(
            paddr.is_page_aligned(),
            "PhysicalPage::from_physical: unaligned address provided"
        );

        Self(paddr)
    }

    pub fn into_physical(self) -> PhysicalMut<[u8; 4096], DirectMapped> {
        self.0
    }
}
