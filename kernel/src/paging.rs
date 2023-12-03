use core::{
    alloc::{AllocError, Allocator, Layout},
    cmp, fmt,
    intrinsics::likely,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    slice,
};

use alloc::{boxed::Box, sync::Arc};
use bitflags::bitflags;

use rille::{
    addr::{
        DirectMapped, Identity, PgOff, Physical, PhysicalConst, PhysicalMut, Ppn, Virtual,
        VirtualConst, VirtualMut, Vpn,
    },
    capability::paging::PageSize,
    units::StorageUnits,
};

use crate::{
    asm::{self, get_satp},
    kalloc::phys::PMAlloc,
    sync::{OnceCell, SpinMutex, SpinRwLock},
};

static ROOT_PAGE_TABLE: OnceCell<SpinMutex<PageTable>> = OnceCell::new();

pub const LOWER_HALF_TOP: usize = 0x003F_FFFF_F000;

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

// TODO: replace all `PageTable`s with `SharedPageTable`s (then rename it back)
#[derive(Debug)]
pub struct PageTable {
    root: Box<RawPageTable, PagingAllocator>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct UserspaceCopyError;

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

    pub fn as_raw(&self) -> &RawPageTable {
        &self.root
    }

    pub fn as_physical(&mut self) -> PhysicalMut<RawPageTable, DirectMapped> {
        Virtual::from_ptr(core::ptr::addr_of_mut!(*self.root)).into_phys()
    }

    pub fn as_physical_const(&self) -> PhysicalConst<RawPageTable, DirectMapped> {
        Virtual::from_ptr(core::ptr::addr_of!(*self.root)).into_phys()
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
    pub fn walk<T>(
        &self,
        virt_addr: VirtualConst<T, Identity>,
    ) -> Option<(PhysicalConst<T, DirectMapped>, PageTableFlags)> {
        let addr = virt_addr.into_usize();
        let page = virt_addr.page_align().into_usize();
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
                    return Some((phys_page, entry.flags));
                }
                PTEKind::Branch(phys_addr) => {
                    // SAFETY: By our invariants, this is safe.
                    table = unsafe { &*(phys_addr.into_virt().into_ptr()) };
                }
                PTEKind::Invalid => return None,
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
                assert!(
		    entry.decode().flags & PageTableFlags::VALID == PageTableFlags::empty(),
		    "PageTable::map: attempted to map an already-mapped vaddr: from={from:#p}, to={to:#p}, flags={flags:?}, existing flags={:?}", entry.decode().flags
		);

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

    /// Try to copy `dst.len()` bytes from the address in this
    /// userspace page table into `dst`.
    ///
    /// The provided `dst` slice will be fully initialized **if and
    /// only if** the function returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// This function will error if the data could not be copied.
    ///
    /// # Safety
    ///
    /// The memory at the provided source address, if accessible by
    /// U-mode, must be initialized.
    pub unsafe fn copy_from_user(
        &self,
        dst: &mut [MaybeUninit<u8>],
        src: VirtualConst<u8, Identity>,
    ) -> Result<(), UserspaceCopyError> {
        let src_page = src.page_align();

        let mut len = dst.len();
        let mut offset = src.into_usize() - src_page.into_usize();
        for page in 0..dst.len().div_ceil(4.kib()) {
            let (phys_addr, flags) = self
                .walk(src_page.add(4.kib() * page + offset))
                .ok_or(UserspaceCopyError)?;
            let virt_addr = phys_addr.into_virt();
            if !flags.contains(PageTableFlags::USER | PageTableFlags::READ | PageTableFlags::VALID)
            {
                return Err(UserspaceCopyError);
            }

            let copy_len = cmp::min(len, 4096);
            let dst_slice_part = &mut dst[(page * 4.kib())..(page * 4.kib() + copy_len)];

            // SAFETY: Our caller guarantees this memory is valid and initialized. Additionally, the layout of `MaybeUninit<u8>` is the same as `u8`.
            unsafe {
                dst_slice_part
                    .copy_from_slice(slice::from_raw_parts(virt_addr.into_ptr().cast(), copy_len));
            }

            len -= copy_len;
            offset = 0;
        }
        debug_assert_eq!(len, 0, "PageTable::copy_from_user: did not copy all bytes");

        Ok(())
    }
}

#[repr(C, align(4096))]
pub struct RawPageTable {
    pub ptes: [RawPageTableEntry; 512],
}

impl fmt::Debug for RawPageTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawPageTable").finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct RawPageTableEntry(pub u64);

impl RawPageTableEntry {
    #[inline(never)]
    pub fn decode(self) -> PageTableEntry {
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageTableEntry {
    pub ppn: Ppn,
    pub flags: PageTableFlags,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

        let ptr = ptr::slice_from_raw_parts_mut(page.into_ptr_mut().cast(), 4096);

        // SAFETY: The pointer is valid for 4096 bytes
        unsafe { &mut *ptr }.fill(0);

        // SAFETY: See above
        Ok(unsafe { NonNull::new_unchecked(ptr) })
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

#[derive(Clone, Debug)]
pub struct SharedPageTable {
    inner: Arc<SpinRwLock<Box<RawPageTable, PagingAllocator>>>,
}

impl PartialEq for SharedPageTable {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for SharedPageTable {}

impl SharedPageTable {
    pub fn from_inner(x: Arc<SpinRwLock<Box<RawPageTable, PagingAllocator>>>) -> Self {
        Self { inner: x }
    }

    pub fn find_free_trapframe_addr(&self) -> Option<VirtualConst<u8, Identity>> {
        let inner = self.inner.read();
        let table = &**inner;
        for l0 in 256..512 {
            if let PTEKind::Branch(paddr) = table.ptes[l0].decode().kind() {
                // SAFETY: By invariants
                let table = unsafe { &*(paddr.into_virt().into_ptr_mut()) };
                for l1 in 0..512 {
                    if let PTEKind::Branch(paddr) = table.ptes[l1].decode().kind() {
                        // SAFETY: By invariants
                        let table = unsafe { &*(paddr.into_virt().into_ptr_mut()) };
                        for l2 in 0..512 {
                            if let PTEKind::Invalid = table.ptes[l2].decode().kind() {
                                return Some(VirtualConst::from_components(
                                    [
                                        Vpn::from_usize_truncate(l2),
                                        Vpn::from_usize_truncate(l1),
                                        Vpn::from_usize_truncate(l0),
                                    ],
                                    None,
                                ));
                            }
                        }
                    } else {
                        return Some(VirtualConst::from_components(
                            [
                                Vpn::from_usize_truncate(0),
                                Vpn::from_usize_truncate(l1),
                                Vpn::from_usize_truncate(l0),
                            ],
                            None,
                        ));
                    }
                }
            } else {
                return Some(VirtualConst::from_components(
                    [
                        Vpn::from_usize_truncate(0),
                        Vpn::from_usize_truncate(0),
                        Vpn::from_usize_truncate(l0),
                    ],
                    None,
                ));
            }
        }

        None
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
    pub fn walk<T>(
        &self,
        virt_addr: VirtualConst<T, Identity>,
    ) -> Option<(PhysicalConst<T, DirectMapped>, PageTableFlags)> {
        let addr = virt_addr.into_usize();
        let page = virt_addr.page_align().into_usize();
        let offset = addr - page;

        let inner = self.inner.read();
        let mut table = &**inner;
        for vpn in virt_addr.vpns().into_iter().rev() {
            let entry = &table.ptes[vpn.into_usize()].decode();

            match entry.kind() {
                PTEKind::Leaf => {
                    let ppn = entry.ppn;
                    let phys_page = PhysicalConst::from_components(
                        ppn,
                        Some(PgOff::from_usize_truncate(offset)),
                    );
                    return Some((phys_page, entry.flags));
                }
                PTEKind::Branch(phys_addr) => {
                    // SAFETY: By our invariants, this is safe.
                    table = unsafe { &*(phys_addr.into_virt().into_ptr()) };
                }
                PTEKind::Invalid => return None,
            }
        }

        unreachable!();
    }

    /// Map a page in this page table.
    ///
    /// # Panics
    ///
    /// This function will panic if either the physical or virtual addresses are unaligned.
    #[track_caller]
    pub fn map(
        &self,
        from: PhysicalConst<u8, Identity>,
        to: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
        size: PageSize,
    ) {
        assert!(
            from.is_page_aligned(),
            "PageTable::map: tried to map from an unaligned physical address: from={from:#p}, to={to:#p}, flags={flags:?}", 
        );
        assert!(
            to.is_page_aligned(),
            "PageTable::map: tried to map to an unaligned virtual address: from={from:#p}, to={to:#p}, flags={flags:?}",
        );

        let depth_max = match size {
            PageSize::Base => 3,
            PageSize::Mega => 2,
            PageSize::Giga => 1,
        };
        let final_iter = match size {
            PageSize::Base => 0,
            PageSize::Mega => 1,
            PageSize::Giga => 2,
        };

        let mut inner = self.inner.write();
        let mut table = &mut **inner;

        for (iter, vpn) in to.vpns().into_iter().enumerate().rev().take(depth_max) {
            let entry = &mut table.ptes[vpn.into_usize()];

            if iter == final_iter {
                // assert!(
                //     entry.decode().flags & PageTableFlags::VALID == PageTableFlags::empty(),
                //     "PageTable::map: attempted to map an already-mapped vaddr: from={from:#p}, to={to:#p}, flags={flags:?}, existing flags={:?}", entry.decode().flags
                // );

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
                    // SAFETY: By invariants
                    table = unsafe { &mut *(paddr.into_virt().into_ptr_mut()) }
                }
                PTEKind::Invalid => {
                    let new_subtable = Box::leak(PageTable::new_table());
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

    pub fn as_physical_const(&self) -> PhysicalConst<RawPageTable, DirectMapped> {
        Virtual::from_ptr(core::ptr::addr_of!(**self.inner.read())).into_phys()
    }
}
