use core::{
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{Identity, Mapping, Mutability, PgOff, Physical, PGOFF_MASK};

/// A virtual address, with a specific [`Mapping`], used to convert it to and
/// from a [`Physical`] address.
///
/// # Internal Structure
///
/// With the Sv39[^1] paging strategy, a virtual address is 39 bits long and is
/// structured as follows:
/// ```plaintext
/// |<-38 30->|<-29 21->|<-20 12->|<-11     0->| bit index
/// |---------|---------|---------|------------|
/// |  VPN[2] |  VPN[1] |  VPN[0] | pg. offset |
/// |---------|---------|---------|------------|
/// |    9    |    9    |    9    |     12     | bit size
/// ```
///
/// Each "VPN" is a virtual page number--a 9-bit index (i.e. `0` to
/// `511`) index into a
/// [`PageTable`](crate::capability::paging::PageTable) tree, where
/// level 2 is the root page table, level 1 is the next level, and
/// level 0 is the final level.
///
/// The "page offset" is the remaining 12 bits, indexing into each 4 KiB
/// (`4096` bytes) page.
///
/// The combination of the VPNs and page offset is able to uniquely address 512
/// GiB of virtual memory.
///
/// To get the page offset, use [`Self::page_offset`]. Similarly, there is a
/// function [`Self::vpns`] that gets the VPNs of a specific virtual address.
///
/// [^1]: See the [module-level documentation][crate::addr] or
///  [the RISCV privileged ISA spec](https://github.com/riscv/riscv-isa-manual/releases/download/draft-20220604-4a01cbb/riscv-privileged.pdf),
///  section 4.4
#[repr(transparent)]
pub struct Virtual<T, Map: Mapping, Mut: Mutability> {
    pub(super) addr: usize,
    pub(super) phantom: PhantomData<(Map, Mut, Mut::RawPointer<T>)>,
}

impl<T, Map: Mapping, Mut: Mutability> Virtual<T, Map, Mut> {
    /// Create a null [`Virtual`] address.
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        Self {
            addr: 0,
            phantom: PhantomData,
        }
    }

    /// Create a [`Virtual`] address from a [`usize`].
    ///
    /// # Panics
    ///
    /// This function will panic if the address is not in the correct address
    /// space.
    #[track_caller]
    #[inline]
    pub fn from_usize(addr: usize) -> Self {
        #[cfg(debug_assertions)]
        {
            match Self::try_from_usize(addr) {
                Some(virt_addr) => virt_addr,
                None => panic!(
                    "Virtual::from_usize: not in address space: addr={:#p}, map={:?}, mut={:?}",
                    addr as *mut u8,
                    Map::default(),
                    Mut::default()
                ),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { Self::from_usize_unchecked(addr) }
        }
    }

    /// Create a [`Virtual`] address from a [`usize`], checking whether it is in
    /// the correct address space.
    #[inline]
    pub fn try_from_usize(addr: usize) -> Option<Self> {
        let addr = canonicalize(addr);
        if !Map::vaddr_space().contains(&addr) {
            return None;
        }

        // SAFETY: We have checked it is in the correct address space.
        Some(unsafe { Self::from_usize_unchecked(addr) })
    }

    /// Create a [`Virtual`] address from a pointer, checking whether it is in
    /// the correct address space.
    #[inline]
    pub fn try_from_ptr(ptr: Mut::RawPointer<T>) -> Option<Self> {
        let addr = Mut::into_usize(ptr);
        Self::try_from_usize(addr)
    }

    /// Create a [`Virtual`] address from a pointer.
    ///
    /// # Panics
    ///
    /// This function will panic if the address is not in the correct address
    /// space.
    #[track_caller]
    #[inline]
    pub fn from_ptr(ptr: Mut::RawPointer<T>) -> Self {
        #[cfg(debug_assertions)]
        {
            match Self::try_from_ptr(ptr) {
                Some(paddr) => paddr,
                None => panic!(
                    "Virtual::from_ptr: not in address space: addr={:#p}, map={:?}, mut={:?}",
                    ptr,
                    Map::default(),
                    Mut::default()
                ),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { Self::from_usize_unchecked(Mut::into_usize(ptr)) }
        }
    }

    /// Create a [`Virtual`] address from a [`usize`], without checking whether
    /// or not it is in the correct address space.
    ///
    /// # Safety
    ///
    /// Only call this function if you are sure that the pointer is within the
    /// valid address space for the given mapping.
    #[inline(always)]
    pub unsafe fn from_usize_unchecked(addr: usize) -> Self {
        Self {
            addr: canonicalize(addr),
            phantom: PhantomData,
        }
    }

    /// Convert a [`Virtual`] address into a [`usize`].
    #[inline(always)]
    pub fn into_usize(self) -> usize {
        self.addr
    }

    /// Get the physical page numbers associated with this [`Virtual`]
    /// address. Note that these are in the order defined by the spec,
    /// i.e. the 0th is most significant, and the 2nd is least
    /// significant.
    // TODO: make this use Vpns
    #[inline]
    pub fn vpns(self) -> [Vpn; 3] {
        // Remove the page offset by shifting 12 bits out.
        let addr_no_pgoff = self.addr >> 12;
        [
            // Get the 0th VPN
            Vpn::from_usize_truncate(addr_no_pgoff),
            // Shift out the 0th VPN, then get the 1st
            Vpn::from_usize_truncate(addr_no_pgoff >> 9),
            // Shift out the 0th and 1st VPN, then get the 2nd
            Vpn::from_usize_truncate(addr_no_pgoff >> (9 * 2)),
        ]
    }

    /// Create an address from virtual page numbers, and optionally, an offset
    /// into the page.
    ///
    /// # Panics
    ///
    /// This function will panic if the resulting address is outside of the
    /// [`Mapping`]'s range.
    #[track_caller]
    #[inline]
    pub fn from_components(vpns: [Vpn; 3], pgoff: Option<PgOff>) -> Self {
        #[cfg(debug_assertions)]
        {
            match Self::try_from_components(vpns, pgoff) {
            Some(vaddr) => vaddr,
            None => panic!("Virtual::from_components: not in address space: vpns={:?}, pgoff={:?}, map={:?}, mut={:?}", vpns, pgoff, Map::default(), Mut::default())
        }
        }
        #[cfg(not(debug_assertions))]
        {
            let [vpn_0, vpn_1, vpn_2] = vpns;
            let vpn_2 = vpn_2.into_usize() << (9 * 2);
            let vpn_1 = vpn_1.into_usize() << 9;
            let vpn_0 = vpn_0.into_usize();
            let pgoff = pgoff.unwrap_or_default().into_usize();
            unsafe { Self::from_usize_unchecked(((vpn_2 | vpn_1 | vpn_0) << 12) | pgoff) }
        }
    }

    /// Create an address from virtual page numbers, and optionally,
    /// an offset into the page. This function will return [`None`] if
    /// the resulting address is outside of the [`Mapping`]'s range.
    #[inline]
    pub fn try_from_components(vpns: [Vpn; 3], pgoff: Option<PgOff>) -> Option<Self> {
        let [vpn_0, vpn_1, vpn_2] = vpns;
        let vpn_2 = vpn_2.into_usize() << (9 * 2);
        let vpn_1 = vpn_1.into_usize() << 9;
        let vpn_0 = vpn_0.into_usize();
        let pgoff = pgoff.unwrap_or_default().into_usize();
        Self::try_from_usize(((vpn_2 | vpn_1 | vpn_0) << 12) | pgoff)
    }

    /// Align a virtual address to the page it is in.
    ///
    /// This is essentially equivalent to
    /// `Virtual::from_components(addr.vpns(), None)`, but more
    /// convenient.
    #[inline]
    #[must_use]
    pub fn page_align(self) -> Virtual<T, Map, Mut> {
        // SAFETY: Page-aligning an address will not
        unsafe { Self::from_usize_unchecked(self.into_usize() & !PGOFF_MASK) }
    }

    /// Returns the page offset of the address.
    #[inline]
    pub fn page_offset(self) -> PgOff {
        PgOff::from_usize_truncate(self.addr)
    }

    /// Returns true if the address is page-aligned, i.e. if the page
    /// offset is 0.
    #[inline]
    pub fn is_page_aligned(self) -> bool {
        self.page_offset().into_usize() == 0
    }

    /// Cast a [`Virtual`] address to another type.
    #[inline]
    pub fn cast<U>(self) -> Virtual<U, Map, Mut> {
        Virtual {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Cast this [`Virtual`] address into an [`Identity`]-mapped
    /// address.
    #[inline]
    pub fn into_identity(self) -> Virtual<T, Identity, Mut> {
        Virtual {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Convert a virtual address into a physical address using its [`Mapping`].
    ///
    /// # Panics
    ///
    /// This function will panic if the address is outside of the [`Mapping`]'s
    /// range.
    #[track_caller]
    #[inline]
    pub fn into_phys(self) -> Physical<T, Map, Mut> {
        #[cfg(debug_assertions)]
        {
            match self.try_into_phys() {
                Some(paddr) => paddr,
                None => panic!(
                    "Virtual::into_virt out of range: self={:#p}, map={:?}, mut={:?}",
                    self,
                    Map::default(),
                    Mut::default()
                ),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { Map::virt2phys_unchecked(self) }
        }
    }

    /// Try to convert a virtual address into a physical address,
    /// returning [`None`] if the address is outside of the
    /// [`Mapping`]'s range.
    #[inline]
    pub fn try_into_phys(self) -> Option<Physical<T, Map, Mut>> {
        Map::virt2phys(self)
    }

    /// Convert this [`Virtual`] address into a constant address.
    #[inline]
    pub fn into_const(self) -> Virtual<T, Map, super::Const> {
        Virtual {
            addr: self.addr,
            phantom: PhantomData,
        }
    }

    /// Convert this [`Virtual`] address into a mutable address.
    #[inline]
    pub fn into_mut(self) -> Virtual<T, Map, super::Mut> {
        Virtual {
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
    #[inline]
    pub fn add(self, by: usize) -> Self {
        #[cfg(debug_assertions)]
        {
            match self.checked_add(by) {
                Some(vaddr) => vaddr,
                None => panic!("Virtual::add out of range: self={:#p}, by={:#x}", self, by),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { Self::from_usize_unchecked(self.into_usize() + by) }
        }
    }

    /// Increment an address `by` bytes, returning [`None`] if the
    /// resulting address is outside of the [`Mapping`]'s range.
    pub fn checked_add(self, by: usize) -> Option<Self> {
        let vaddr = self.into_usize();
        let vaddr = vaddr.checked_add(by)?;
        Self::try_from_usize(vaddr)
    }

    /// Convert a [`Virtual`] address into a [`*const T`].
    #[inline]
    pub fn into_ptr(self) -> *const T {
        self.addr as *const _
    }

    /// Convert a [`Virtual`] address into a [`*mut T`].
    #[inline]
    pub const fn into_ptr_mut(self) -> *mut T {
        self.addr as *mut _
    }
}

impl<T, Map: Mapping, Mut: Mutability> fmt::Pointer for Virtual<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#p}", self.addr as *mut u8)
    }
}

impl<T, Map: Mapping, Mut: Mutability> From<u64> for Virtual<T, Map, Mut> {
    #[inline]
    fn from(x: u64) -> Virtual<T, Map, Mut> {
        Self::from_usize(x as usize)
    }
}

impl<T, Map: Mapping, Mut: Mutability> From<usize> for Virtual<T, Map, Mut> {
    #[inline]
    fn from(x: usize) -> Virtual<T, Map, Mut> {
        Self::from_usize(x)
    }
}

impl<T, Map: Mapping, Mut: Mutability> From<Virtual<T, Map, Mut>> for u64 {
    #[inline]
    fn from(x: Virtual<T, Map, Mut>) -> u64 {
        x.into_usize() as u64
    }
}

impl<T, Map: Mapping, Mut: Mutability> From<Virtual<T, Map, Mut>> for usize {
    #[inline]
    fn from(x: Virtual<T, Map, Mut>) -> usize {
        x.into_usize()
    }
}

/// A virtual page number. See [`Virtual`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Vpn(u16);

/// A 9-bit mask.
const VPN_MASK: usize = 0b1_1111_1111;

impl Vpn {
    /// Convert a [`usize`] into a `Vpn`, truncating any extraneous
    /// bits as necessary.
    pub fn from_usize_truncate(vpn: usize) -> Self {
        Self((vpn & VPN_MASK) as u16)
    }

    /// Convert a Vpn into a [`usize`].
    #[inline]
    pub fn into_usize(self) -> usize {
        self.0 as usize
    }

    /// Convert a Vpn into a [`u16`].
    #[inline(always)]
    pub fn into_u16(self) -> u16 {
        self.0
    }
}

impl From<Vpn> for u16 {
    #[inline]
    fn from(x: Vpn) -> u16 {
        x.into_u16()
    }
}

impl From<u16> for Vpn {
    #[inline]
    fn from(x: u16) -> Vpn {
        Vpn::from_usize_truncate(x as usize)
    }
}

impl From<u64> for Vpn {
    #[inline]
    fn from(x: u64) -> Vpn {
        Vpn::from_usize_truncate(x as usize)
    }
}

impl From<Vpn> for u64 {
    #[inline]
    fn from(x: Vpn) -> u64 {
        x.into_u16() as u64
    }
}

/// A collection of virtual page numbers ([`Vpn`]s) that fully define
/// the virtual address of some page in memory. Note that these are in
/// the order defined in the spec, meaning that the 0th [`Vpn`] is
/// most significant, and the 2nd [`Vpn`] is the least significant.
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct Vpns(pub [Vpn; 3]);

impl fmt::Debug for Vpns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Vpns").field(&u32::from(*self)).finish()
    }
}

impl Deref for Vpns {
    type Target = [Vpn; 3];

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Vpns {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vpns> for u32 {
    #[inline]
    fn from(x: Vpns) -> u32 {
        let [vpn0, vpn1, vpn2] = &*x;
        let vpn2 = vpn2.into_u16() as u32;
        let vpn1 = vpn1.into_u16() as u32;
        let vpn0 = vpn0.into_u16() as u32;
        (vpn2 << (9 * 2)) | (vpn1 << 9) | vpn0
    }
}

impl From<u32> for Vpns {
    #[inline]
    fn from(x: u32) -> Vpns {
        let x = x as usize;
        Vpns([
            Vpn::from_usize_truncate(x),
            Vpn::from_usize_truncate(x >> 9),
            Vpn::from_usize_truncate(x >> (9 * 2)),
        ])
    }
}

/// The top bit of a 39-bit address
const TOP_BIT: usize = 1 << 38;

/// Some implementations may require that the topmost bits of an
/// address be equal, up to and including the topmost bit used by the
/// paging algorithm.  This function performs this transformation
/// (sometimes called "canonical addresses")
#[inline]
pub fn canonicalize(mut addr: usize) -> usize {
    // is the top bit set?
    if addr & TOP_BIT != 0 {
        // set topmost bits
        addr |= 0xFFFF_FFC0_0000_0000;
    }

    addr
}

/// Inverse of [`canonicalize`]. This function will not recover lost
/// data, but will simply zero out the top bits, allowing tagged
/// pointers to work.
#[inline]
pub fn decanonicalize(addr: usize) -> usize {
    // clear bits 40 to 63 by shifting them out and back in (we're a
    // usize so there's no change we sign-extend here)
    (addr << (64 - 39)) >> (64 - 39)
}
