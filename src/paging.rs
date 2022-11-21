use core::{
    alloc::{AllocError, Allocator, Layout},
    fmt,
    intrinsics::likely,
    mem::MaybeUninit,
    ptr::{self, NonNull},
};

use alloc::boxed::Box;
use bitflags::bitflags;

use crate::{
    addr::{
        DirectMapped, Identity, PgOff, Physical, PhysicalConst, PhysicalMut, Ppn, Virtual,
        VirtualConst, VirtualMut,
    },
    asm::{self, get_satp},
    kalloc::phys::PMAlloc,
    once_cell::OnceCell,
    spin::SpinMutex,
    units::StorageUnits,
};

static ROOT_PAGE_TABLE: OnceCell<SpinMutex<PageTable>> = OnceCell::new();

/// Get the root page table.
///
/// # Panics
///
/// This function will panic if paging is not enabled.
#[track_caller]
pub fn root_page_table() -> &'static SpinMutex<PageTable> {
    assert!(enabled(), "paging::root_page_table: paging must be enabled");
    ROOT_PAGE_TABLE.get_or_init(|| {
        let satp = get_satp().decode();
        let ppn = satp.ppn;
        let phys_addr: PhysicalMut<_, DirectMapped> = PhysicalMut::from_components(ppn, None);
        let virt_addr = phys_addr.into_virt();
        // SAFETY: The pointer is valid and we have an exclusive reference to it.
        let pgtbl = unsafe { PageTable::from_raw(virt_addr) };
        SpinMutex::new(pgtbl)
    })
}

#[derive(Debug)]
pub struct PageTable {
    root: Box<RawPageTable, PagingAllocator>,
}

impl PageTable {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            root: Self::new_table(),
        }
    }

    /// Leak the inner [`RawPageTable`]. To clean this up, use [`Self::from_raw`].
    pub fn into_raw(self) -> VirtualMut<RawPageTable, DirectMapped> {
        let ptr = Box::into_raw(self.root);
        Virtual::from_ptr(ptr)
    }

    /// Recover a leaked [`PageTable`] from [`Self::into_raw`].
    ///
    /// # Safety
    ///
    /// The `root` pointer must be obtained from [`Self::into_raw`].
    pub unsafe fn from_raw(root: VirtualMut<RawPageTable, DirectMapped>) -> Self {
        Self {
            // SAFETY: Our caller guarantees this is safe.
            root: unsafe { Box::from_raw_in(root.into_ptr_mut(), PagingAllocator) },
        }
    }

    pub fn as_physical(&mut self) -> PhysicalMut<RawPageTable, DirectMapped> {
        Virtual::from_ptr(core::ptr::addr_of_mut!(*self.root)).into_phys()
    }

    fn new_table() -> Box<RawPageTable, PagingAllocator> {
        // SAFETY: The page is zeroed and thus is well-defined and valid.
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) }
    }

    /// Walk a virtual address (which does not have to be page-aligned) and find
    /// the physical address it maps to (which is offset from the page by the
    /// same amount as the virtual address).
    ///
    /// # Panics
    ///
    /// This function will panic if it is called with an unmapped virtual
    /// address.
    #[track_caller]
    pub fn walk(&self, virt_addr: VirtualConst<u8, Identity>) -> PhysicalConst<u8, Identity> {
        let addr = virt_addr.into_usize();
        let page = addr.next_multiple_of(4096) - 4096;
        let offset = addr - page;

        let mut table = &*self.root;

        for vpn in virt_addr.vpns().into_iter().rev() {
            let entry = &table.ptes[vpn.into_usize()].decode();

            match entry.kind() {
                PTEKind::Leaf => {
                    let ppn = entry.ppn;
                    let phys_page = PhysicalConst::from_components(
                        ppn,
                        Some(PgOff::from_usize_truncate(offset)),
                    );
                    return phys_page;
                }
                PTEKind::Branch(phys_addr) => {
                    // SAFETY: By our invariants, this is safe.
                    table = unsafe { &*(phys_addr.into_virt().into_ptr()) }
                }
                PTEKind::Invalid => panic!(
                    "PageTable::walk: attempted to walk an unmapped vaddr: vaddr={:#p}, flags={:?}",
                    virt_addr, entry.flags
                ),
            }
        }

        unreachable!();
    }

    /// Map a gibibyte (GiB)-sized page in this page table.
    ///
    /// # Panics
    ///
    /// This function will panic if either the physical or virtual addresses are unaligned.
    #[track_caller]
    pub fn map_gib(
        &mut self,
        from: PhysicalConst<u8, Identity>,
        to: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) {
        assert_eq!(
            from.into_usize() % 1.gib(),
            0,
            "PageTable::map_gib: tried to map from an unaligned physical address: from={from:#p}, to={to:#p}, flags={flags:?}"
        );
        assert_eq!(
            to.into_usize() % 1.gib(),
            0,
            "PageTable::map_gib: tried to map to an unaligned virtual address: from={from:#p}, to={to:#p}, flags={flags:?}",
        );

        let table = &mut *self.root;
        let vpn = to.vpns()[2];
        let entry = &mut table.ptes[vpn.into_usize()];
        entry.update_in_place(|mut pte| {
            pte.flags = flags;
            pte.ppn = from.ppn();
            pte
        });
    }

    /// Map a page in this page table.
    ///
    /// # Panics
    ///
    /// This function will panic if either the physical or virtual addresses are unaligned.
    #[track_caller]
    pub fn map(
        &mut self,
        from: PhysicalConst<u8, Identity>,
        to: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) {
        assert!(
            from.is_page_aligned(),
            "PageTable::map: tried to map from an unaligned physical address: from={from:#p}, to={to:#p}, flags={flags:?}", 
        );
        assert!(
            to.is_page_aligned(),
            "PageTable::map: tried to map to an unaligned virtual address: from={from:#p}, to={to:#p}, flags={flags:?}",
        );

        let mut table = &mut *self.root;

        for (lvl, vpn) in to.vpns().into_iter().enumerate().rev() {
            let entry = &mut table.ptes[vpn.into_usize()];

            if lvl == 0 {
                assert!(entry.decode().flags & PageTableFlags::VALID == PageTableFlags::empty(), "PageTable::map: attempted to map an already-mapped vaddr: from={from:#p}, to={to:#p}, flags={flags:?}, existing flags={:?}", entry.decode().flags);

                entry.update_in_place(|mut pte| {
                    pte.flags = flags;
                    pte.ppn = from.ppn();
                    pte
                });

                return;
            }

            match entry.decode().kind() {
                PTEKind::Leaf => unreachable!(),
                PTEKind::Branch(paddr) => {
                    // SAFETY: By our invariants, this is safe.
                    table = unsafe { &mut *(paddr.into_virt().into_ptr_mut()) }
                }
                PTEKind::Invalid => {
                    let new_subtable = Box::leak(Self::new_table());
                    let subtable_phys: PhysicalMut<RawPageTable, DirectMapped> =
                        Virtual::from_ptr(new_subtable as *mut _).into_phys();
                    entry.update_in_place(|mut pte| {
                        pte.flags = PageTableFlags::VALID;
                        pte.ppn = subtable_phys.ppn();
                        pte
                    });

                    table = new_subtable;
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C, align(4096))]
pub struct RawPageTable {
    pub ptes: [RawPageTableEntry; 512],
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct RawPageTableEntry(u64);

unsafe fn subtable_ref(ppn: Ppn) -> &'static RawPageTable {
    let phys_addr: PhysicalMut<_, DirectMapped> = Physical::from_components(ppn, None);
    let virt_addr = phys_addr.into_virt();
    // TODO: make this actually properly safe
    unsafe { &*virt_addr.into_ptr() }
}

#[derive(Copy, Clone)]
#[repr(transparent)]
struct KindFormatter(PageTableEntry);
impl fmt::Debug for KindFormatter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.kind() {
            PTEKind::Leaf => f.debug_tuple("PTEKind::Leaf").field(&self.0.ppn).finish(),
            PTEKind::Branch(_) => f
                .debug_struct("PTEKind::Branch")
                .field("phys_addr", &self.0.ppn)
                .field("subtable", &unsafe { subtable_ref(self.0.ppn) })
                .finish(),
            PTEKind::Invalid => f.debug_tuple("PTEKind::Invalid").finish(),
        }
    }
}

impl fmt::Debug for RawPageTableEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decoded = self.decode();

        f.debug_struct("RawPageTableEntry")
            .field("as_raw", &self.0)
            .field("decoded", &decoded)
            .finish()
    }
}

impl RawPageTableEntry {
    pub const fn decode(self) -> PageTableEntry {
        PageTableEntry {
            ppn: Ppn::from_usize_truncate((self.0 >> 10) as usize),
            flags: PageTableFlags::from_bits_truncate(self.0 as u8),
        }
    }

    #[must_use]
    pub fn update(self, f: impl FnOnce(PageTableEntry) -> PageTableEntry) -> RawPageTableEntry {
        f(self.decode()).encode()
    }

    pub fn update_in_place(&mut self, f: impl FnOnce(PageTableEntry) -> PageTableEntry) {
        f(self.decode()).encode_to(self);
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct PageTableEntry {
    pub ppn: Ppn,
    pub flags: PageTableFlags,
}

impl fmt::Debug for PageTableEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageTableEntry")
            .field("flags", &self.flags)
            .field("kind", &KindFormatter(*self))
            .finish()
    }
}

pub enum PTEKind {
    Leaf,
    Branch(PhysicalMut<RawPageTable, DirectMapped>),
    Invalid,
}

impl PageTableEntry {
    pub fn encode(self) -> RawPageTableEntry {
        let addr = (self.ppn.into_usize() << 10) as u64;
        let flags = self.flags.bits() as u64;
        RawPageTableEntry(addr | flags)
    }

    pub fn kind(self) -> PTEKind {
        if (self.flags & PageTableFlags::VALID) == PageTableFlags::empty() {
            return PTEKind::Invalid;
        }

        if (self.flags & PageTableFlags::RWX) == PageTableFlags::empty() {
            return PTEKind::Branch(Physical::from_components(self.ppn, None));
        }

        PTEKind::Leaf
    }

    #[inline]
    pub fn encode_to(self, rpte: &mut RawPageTableEntry) {
        *rpte = self.encode();
    }
}

bitflags! {
    pub struct PageTableFlags: u8 {
        const VALID = 1 << 0;
        const READ = 1 << 1;
        const WRITE = 1 << 2;
        const EXECUTE = 1 << 3;
        const USER = 1 << 4;
        const GLOBAL = 1 << 5;
        const ACCESSED = 1 << 6;
        const DIRTY = 1 << 7;
        const RW = Self::READ.bits | Self::WRITE.bits;
        const RX = Self::READ.bits | Self::EXECUTE.bits;
        const RWX = Self::READ.bits | Self::WRITE.bits | Self::EXECUTE.bits;
        const VAD = Self::ACCESSED.bits | Self::DIRTY.bits | Self::VALID.bits;
    }
}

pub struct PagingAllocator;

// SAFETY:
// - This allocator always provides valid addresses and never double-allocates.
// - This allocator cannot be cloned.
// - Addresses allocated with this allocator can be freed by it.
unsafe impl Allocator for PagingAllocator {
    #[track_caller]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        assert_eq!(
            layout.align(),
            4096,
            "PagingAllocator::allocate the paging allocator must allocate 4KiB blocks"
        );

        let page = PMAlloc::get().allocate(0).ok_or(AllocError)?;
        let page = page.into_virt();

        // SAFETY: The pointer is valid for 4096 bytes
        Ok(unsafe {
            NonNull::new_unchecked(ptr::slice_from_raw_parts_mut(
                page.into_ptr_mut().cast(),
                4096,
            ))
        })
    }

    #[track_caller]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        assert_eq!(
            layout.align(),
            4096,
            "PagingAllocator::allocate the paging allocator must allocate 4KiB blocks"
        );

        let virt_addr: VirtualMut<u8, DirectMapped> = Virtual::from_ptr(ptr.as_ptr());
        let phys_addr = virt_addr.into_phys();

        // SAFETY: Our caller guarantees this is safe.
        unsafe { PMAlloc::get().deallocate(phys_addr, 0) }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Satp {
    pub asid: u16,
    pub ppn: Ppn,
}

impl Satp {
    pub fn encode(self) -> RawSatp {
        let ppn = self.ppn.into_usize();
        // 8 = Sv39
        let mode = 8 << 60;
        RawSatp(mode | ((self.asid as u64) << 44) | (ppn as u64))
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct RawSatp(u64);

impl RawSatp {
    pub fn new_unchecked(x: u64) -> RawSatp {
        Self(x)
    }

    #[cfg_attr(debug_assertions, track_caller)]
    pub fn decode(self) -> Satp {
        debug_assert_eq!(self.0 >> 60, 8, "RawSatp::decode: invalid SATP mode");
        // TODO: asid newtype
        let asid = (self.0 >> 44) & 0xFFFF;
        Satp {
            asid: asid as u16,
            ppn: Ppn::from_usize_truncate(self.0 as usize),
        }
    }

    pub fn mode(self) -> u8 {
        (self.0 >> 60) as u8
    }

    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

#[inline(always)]
pub fn enabled() -> bool {
    let satp = asm::get_satp();
    // Except for the small point before paging is enabled at boot,
    // this should be true. (Not sure if this helps *too* much with
    // perf, but can't hurt).
    likely(satp.mode() == 8)
}
