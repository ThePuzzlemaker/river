use core::{
    alloc::{AllocError, Allocator, Layout},
    fmt,
    mem::MaybeUninit,
    ptr::{self, NonNull},
};

use alloc::boxed::Box;
use bitflags::bitflags;

use crate::{
    addr::{
        DirectMapped, Identity, Physical, PhysicalConst, PhysicalMut, Ppn, Virtual, VirtualConst,
        VirtualMut,
    },
    asm::{self, get_satp},
    kalloc::phys::PMAlloc,
    once_cell::OnceCell,
    spin::SpinMutex,
    units::StorageUnits,
};

static ROOT_PAGE_TABLE: OnceCell<SpinMutex<PageTable>> = OnceCell::new();

/// Do not call before paging is enabled.
#[track_caller]
pub fn root_page_table() -> &'static SpinMutex<PageTable> {
    assert!(enabled(), "paging::root_page_table: paging must be enabled");
    ROOT_PAGE_TABLE.get_or_init(|| {
        let satp = get_satp().decode();
        let ppn = satp.ppn;
        let paddr: PhysicalMut<_, DirectMapped> = PhysicalMut::from_components(ppn, None);
        let vaddr = paddr.into_virt();
        // SAFETY: The pointer is valid and we have an exclusive reference to it.
        let pgtbl = unsafe { PageTable::from_raw(vaddr) };
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
            root: Box::from_raw_in(root.into_ptr_mut(), PagingAllocator),
        }
    }

    pub fn as_physical(&mut self) -> PhysicalMut<RawPageTable, DirectMapped> {
        Virtual::from_ptr(&mut *self.root as *mut _).into_phys()
    }

    fn new_table() -> Box<RawPageTable, PagingAllocator> {
        // SAFETY: The page is zeroed and thus is well-defined and valid.
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) }
    }

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
            "PageTable::map_gib: tried to map from an unaligned physical address: from={:#p}, to={:#p}, flags={:?}", from, to, flags
        );
        assert_eq!(
            to.into_usize() % 1.gib(),
            0,
            "PageTable::map_gib: tried to map to an unaligned virtual address: from={:#p}, to={:#p}, flags={:?}",
            from,
            to,
            flags
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

    #[track_caller]
    pub fn map(
        &mut self,
        from: PhysicalConst<u8, Identity>,
        to: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) {
        assert!(
            from.is_page_aligned(),
            "PageTable::map: tried to map from an unaligned physical address: from={:#p}, to={:#p}, flags={:?}", from, to, flags
        );
        assert!(
            to.is_page_aligned(),
            "PageTable::map: tried to map to an unaligned virtual address: from={:#p}, to={:#p}, flags={:?}",
            from,
            to,
            flags
        );

        let mut table = &mut *self.root;

        for (lvl, vpn) in to.vpns().into_iter().enumerate().rev() {
            let entry = &mut table.ptes[vpn.into_usize()];

            if lvl == 0 {
                if entry.decode().flags & PageTableFlags::VALID != PageTableFlags::empty() {
                    panic!("PageTable::map: attempted to map an already-mapped vaddr: from={:#p}, to={:#p}, flags={:?}, existing flags={:?}", from, to, flags, entry.decode().flags);
                }

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

fn subtable_ref(ppn: Ppn) -> &'static RawPageTable {
    let paddr: PhysicalMut<_, DirectMapped> = Physical::from_components(ppn, None);
    let vaddr = paddr.into_virt();
    // TODO: make this actually properly safe
    unsafe { &*vaddr.into_ptr() }
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
                .field("subtable", &subtable_ref(self.0.ppn))
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

    pub fn update(self, f: impl FnOnce(PageTableEntry) -> PageTableEntry) -> RawPageTableEntry {
        f(self.decode()).encode()
    }

    pub fn update_in_place(&mut self, f: impl FnOnce(PageTableEntry) -> PageTableEntry) {
        f(self.decode()).encode_to(self)
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
        const RW = Self::READ.bits | Self::WRITE.bits;
        const RX = Self::READ.bits | Self::EXECUTE.bits;
        const RWX = Self::READ.bits | Self::WRITE.bits | Self::EXECUTE.bits;
    }
}

pub struct PagingAllocator;

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

        let vaddr: VirtualMut<u8, DirectMapped> = Virtual::from_ptr(ptr.as_ptr());
        let paddr = vaddr.into_phys();

        PMAlloc::get().deallocate(paddr, 0)
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

#[inline]
pub fn enabled() -> bool {
    let satp = asm::get_satp();
    satp.mode() == 8
}
