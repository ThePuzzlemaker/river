use core::{fmt, marker::PhantomData};

use super::{Identity, Mapping, Mutability, PgOff, Virtual};

// TODO: maybe nonnull of physical & virtual ptrs?
// TODO: document physical addr structure
#[repr(transparent)]
pub struct Physical<T, Map: Mapping, Mut: Mutability<T>> {
    pub(super) addr: usize,
    pub(super) _phantom: PhantomData<(Map, Mut, Mut::RawPointer)>,
}

impl<T, Map: Mapping, Mut: Mutability<T>> Physical<T, Map, Mut> {
    pub const NULL: Self = Self {
        addr: 0,
        _phantom: PhantomData,
    };

    /// Create a [`Physical`] address from a [`usize`].
    ///
    /// # Panics
    ///
    /// This function will panic if the address is not in the correct address
    /// space.
    #[track_caller]
    pub fn from_usize(addr: usize) -> Self {
        match Self::try_from_usize(addr) {
            Some(paddr) => paddr,
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
    pub fn try_from_ptr(ptr: Mut::RawPointer) -> Option<Self> {
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
    pub fn from_ptr(ptr: Mut::RawPointer) -> Self {
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
            _phantom: PhantomData,
        }
    }

    #[inline(always)]
    pub fn into_usize(self) -> usize {
        self.addr
    }

    #[inline(always)]
    pub const fn null() -> Self {
        Self {
            addr: 0,
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn is_null(self) -> bool {
        self.addr == 0
    }

    #[inline(always)]
    pub fn cast<U>(self) -> Physical<U, Map, Mut>
    where
        Mut: Mutability<U>,
    {
        Physical {
            addr: self.addr,
            _phantom: PhantomData,
        }
    }

    #[allow(clippy::should_implement_trait)]
    #[track_caller]
    pub fn add(self, by: usize) -> Self {
        match self.checked_add(by) {
            Some(paddr) => paddr,
            None => panic!("Physical::add out of range: self={:#p}, by={:#x}", self, by),
        }
    }

    pub fn checked_add(self, by: usize) -> Option<Self> {
        let paddr = self.into_usize();
        let paddr = paddr.checked_add(by)?;
        Self::try_from_usize(paddr)
    }

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

    pub fn try_into_virt(self) -> Option<Virtual<T, Map, Mut>> {
        Map::phys2virt(self)
    }

    #[inline]
    pub fn is_page_aligned(self) -> bool {
        self.page_offset().into_usize() == 0
    }

    #[inline]
    pub fn ppn(self) -> Ppn {
        // Shift out the page offset, then put mask out the PPN
        Ppn::from_usize_truncate(self.addr >> 12)
    }

    #[inline]
    pub fn page_offset(self) -> PgOff {
        PgOff::from_usize_truncate(self.addr)
    }

    #[track_caller]
    pub fn from_components(ppn: Ppn, pgoff: Option<PgOff>) -> Self {
        match Self::try_from_components(ppn, pgoff) {
            Some(paddr) => paddr,
            None => panic!("Physical::from_components: not in address space: ppn={:?}, pgoff={:?}, map={:?}, mut={:?}", ppn, pgoff, Map::default(), Mut::default())
        }
    }

    pub fn try_from_components(ppn: Ppn, pgoff: Option<PgOff>) -> Option<Self> {
        let ppn = ppn.into_usize() << 12;
        let pgoff = pgoff.unwrap_or_default().into_usize();
        Self::try_from_usize(ppn | pgoff)
    }

    #[inline]
    pub fn into_identity(self) -> Physical<T, Identity, Mut> {
        Physical {
            addr: self.addr,
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn into_mut(self) -> Physical<T, Map, super::Mut> {
        Physical {
            addr: self.addr,
            _phantom: PhantomData,
        }
    }

    #[inline]
    pub fn into_const(self) -> Physical<T, Map, super::Const> {
        Physical {
            addr: self.addr,
            _phantom: PhantomData,
        }
    }
}

impl<T, Map: Mapping, Mut: Mutability<T>> fmt::Pointer for Physical<T, Map, Mut> {
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
    pub const fn from_usize_truncate(ppn: usize) -> Self {
        Self(ppn & PPN_MASK)
    }

    pub fn into_usize(self) -> usize {
        self.0 as usize
    }
}
