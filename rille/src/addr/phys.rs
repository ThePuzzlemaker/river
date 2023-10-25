use core::{fmt, marker::PhantomData};

use super::{Identity, Mapping, Mutability, PgOff, Virtual};

/// A physical address, with a specific [`Mapping`], used to convert it to and
/// from a [`Virtual`] address.
///
/// # Internal Structure
///
/// With the Sv39[^1] paging strategy, a physical address is 56 bits long and is
/// structured as follows:
/// ```plaintext
/// |<-55                      12->|<-11     0->| bit index
/// |------------------------------|------------|
/// |     physical page number     | pg. offset |
/// |------------------------------|------------|
/// |               44             |     12     | bit size
/// ```
///
/// The physical page number (also called a "PPN") is a unique 44-bit
/// page-aligned identifier to a page in physical memory.
///
/// The "page offset" is the remaining 12 bits, indexing into each 4 KiB
/// (`4096` bytes) page.
///
/// To get the page offset, use [`Self::page_offset`]. Similarly, there is a
/// function [`Self::ppn`] that gets the PPN of a specific physical address.
///
/// [^1]: See the [module-level documentation][crate::addr] or
///  [the RISCV privileged ISA spec](https://github.com/riscv/riscv-isa-manual/releases/download/draft-20220604-4a01cbb/riscv-privileged.pdf),
///  section 4.4
#[repr(transparent)]
pub struct Physical<T, Map: Mapping, Mut: Mutability> {
    pub(super) addr: usize,
    pub(super) phantom: PhantomData<(Map, Mut, Mut::RawPointer<T>)>,
}

impl<T, Map: Mapping, Mut: Mutability> Physical<T, Map, Mut> {
    /// Create a [`Physical`] address from a [`usize`].
    ///
    /// # Panics
    ///
    /// This function will panic if the address is not in the correct address
    /// space.
    #[track_caller]
    pub fn from_usize(addr: usize) -> Self {
        match Self::try_from_usize(addr) {
            Some(phys_addr) => phys_addr,
            None => panic!(
                "Physical::from_usize: not in address space: addr={:#p}, map={:?}, mut={:?}",
                addr as *mut u8,
                Map::default(),
                Mut::default()
            ),
        }
    }

    /// Create a [`Physical`] address from a [`usize`], checking whether it is in
    /// the correct address space.
    pub fn try_from_usize(addr: usize) -> Option<Self> {
        if !Map::paddr_space().contains(&addr) {
            return None;
        }

        // SAFETY: We have checked it is in the correct address space.
        Some(unsafe { Self::from_usize_unchecked(addr) })
    }

    /// Create a [`Physical`] address from a pointer, checking whether it is in
    /// the correct address space.
    pub fn try_from_ptr(ptr: Mut::RawPointer<T>) -> Option<Self> {
        let addr = Mut::into_usize(ptr);
        Self::try_from_usize(addr)
    }

    /// Create a [`Physical`] address from a pointer.
    ///
    /// # Panics
    ///
    /// This function will panic if the address is not in the correct address
    /// space.
    #[track_caller]
    pub fn from_ptr(ptr: Mut::RawPointer<T>) -> Self {
        match Self::try_from_ptr(ptr) {
            Some(paddr) => paddr,
            None => panic!(
                "Physical::from_ptr: not in address space: addr={:#p}, map={:?}, mut={:?}",
                ptr,
                Map::default(),
                Mut::default()
            ),
        }
    }

    /// Create a [`Physical`] address from a [`usize`], without checking whether
    /// or not it is in the correct address space.
    ///
    /// # Safety
    ///
    /// Only call this function if you are sure that the pointer is within the
    /// valid address space for the given mapping.
    #[inline(always)]
    pub unsafe fn from_usize_unchecked(addr: usize) -> Self {
        Self {
            addr,
            phantom: PhantomData,
        }
    }

    /// Convert a [`Physical`] address into a [`usize`].
    #[inline(always)]
    pub fn into_usize(self) -> usize {
        self.addr
    }

    /// Create a null [`Physical`] address.
    #[inline(always)]
    #[must_use]
    pub const fn null() -> Self {
        Self {
            addr: 0,
            phantom: PhantomData,
        }
    }

    /// Returns `true` if this [`Physical`] address is null.
    #[inline]
    pub fn is_null(self) -> bool {
        self.addr == 0
    }

    /// Cast a [`Physical`] address into an address of a different type.
    #[inline(always)]
    pub fn cast<U>(self) -> Physical<U, Map, Mut> {
        Physical {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Increment an address `by` bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if the resulting address is outside of the
    /// [`Mapping`]'s range.
    #[allow(clippy::should_implement_trait)]
    #[track_caller]
    #[must_use]
    pub fn add(self, by: usize) -> Self {
        match self.checked_add(by) {
            Some(paddr) => paddr,
            None => panic!("Physical::add out of range: self={:#p}, by={:#x}", self, by),
        }
    }

    /// Increment an address `by` bytes, returning [`None`] if the
    /// resulting address is outside of the [`Mapping`]'s range.
    pub fn checked_add(self, by: usize) -> Option<Self> {
        let paddr = self.into_usize();
        let paddr = paddr.checked_add(by)?;
        Self::try_from_usize(paddr)
    }

    /// Convert this address into a [`Virtual`] address using its [`Mapping`].
    ///
    /// # Panics
    ///
    /// This function will panic if the address is outside of the [`Mapping`]'s
    /// range.
    #[track_caller]
    pub fn into_virt(self) -> Virtual<T, Map, Mut> {
        match self.try_into_virt() {
            Some(vaddr) => vaddr,
            None => panic!(
                "Physical::into_virt out of range: self={:#p}, map={:?}, mut={:?}",
                self,
                Map::default(),
                Mut::default()
            ),
        }
    }

    /// Try to convert this address into a [`Virtual`] address,
    /// returning [`None`] if the address is outside of the
    /// [`Mapping`]'s range.
    pub fn try_into_virt(self) -> Option<Virtual<T, Map, Mut>> {
        Map::phys2virt(self)
    }

    /// Returns true if this [`Physical`] address is page-aligned,
    /// i.e. its page offset is 0.
    #[inline]
    pub fn is_page_aligned(self) -> bool {
        self.page_offset().into_usize() == 0
    }

    /// Returns the physical page number of a [`Physical`] address.
    #[inline]
    pub fn ppn(self) -> Ppn {
        // Shift out the page offset, then put mask out the PPN
        Ppn::from_usize_truncate(self.addr >> 12)
    }

    /// Returns the page offset of a [`Physical`] address.
    #[inline]
    pub fn page_offset(self) -> PgOff {
        PgOff::from_usize_truncate(self.addr)
    }

    /// Create an address from a physical page number and, optionally, an offset
    /// into the page.
    ///
    /// # Panics
    ///
    /// This function will panic if the address is outside of the [`Mapping`]'s
    /// range.
    #[track_caller]
    pub fn from_components(ppn: Ppn, pgoff: Option<PgOff>) -> Self {
        match Self::try_from_components(ppn, pgoff) {
            Some(paddr) => paddr,
            None => panic!("Physical::from_components: not in address space: ppn={:?}, pgoff={:?}, map={:?}, mut={:?}", ppn, pgoff, Map::default(), Mut::default())
        }
    }

    /// Try to create an address from a physical page number and,
    /// optionally, an offset into the page. This function will return
    /// [`None`] if the address is outside of the [`Mapping`]'s range.
    pub fn try_from_components(ppn: Ppn, pgoff: Option<PgOff>) -> Option<Self> {
        let ppn = ppn.into_usize() << 12;
        let pgoff = pgoff.unwrap_or_default().into_usize();
        Self::try_from_usize(ppn | pgoff)
    }

    /// Cast this [`Physical`] address into an identity-mapped
    /// address.
    #[inline]
    pub fn into_identity(self) -> Physical<T, Identity, Mut> {
        Physical {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Cast this [`Physical`] address into a mutable address.
    #[inline]
    pub fn into_mut(self) -> Physical<T, Map, super::Mut> {
        Physical {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Cast this [`Physical`] address into a constant address.
    #[inline]
    pub fn into_const(self) -> Physical<T, Map, super::Const> {
        Physical {
            addr: self.addr,
            phantom: PhantomData,
        }
    }
}

impl<T, Map: Mapping, Mut: Mutability> fmt::Pointer for Physical<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#p}", self.addr as *mut u8)
    }
}

/// A 44-bit physical page number. See [`Physical`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ppn(usize);

/// A 44-bit bitmask (11 `F`s, times 4 bits each = 44 bits)
const PPN_MASK: usize = 0x0FFF_FFFF_FFFF;

impl Ppn {
    /// Convert a usize into a [`Ppn`], truncating extraneous bits as
    /// necessary.
    pub const fn from_usize_truncate(ppn: usize) -> Self {
        Self(ppn & PPN_MASK)
    }

    /// Convert a [`Ppn`] into a [`usize`].
    pub fn into_usize(self) -> usize {
        self.0
    }
}

impl From<u64> for Ppn {
    fn from(x: u64) -> Ppn {
        Ppn::from_usize_truncate(x as usize)
    }
}

impl From<Ppn> for u64 {
    fn from(x: Ppn) -> u64 {
        x.into_usize() as u64
    }
}

impl From<usize> for Ppn {
    fn from(x: usize) -> Ppn {
        Ppn::from_usize_truncate(x)
    }
}

impl From<Ppn> for usize {
    fn from(x: Ppn) -> usize {
        x.into_usize()
    }
}
