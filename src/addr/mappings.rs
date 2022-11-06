use core::{
    fmt,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::symbol::{kernel_end, kernel_start};
use crate::units::StorageUnits;

use super::{Mutability, Physical, Virtual};

pub const KERNEL_OFFSET: usize = 0xFFFF_FFD0_0000_0000;
pub const ACTUAL_PHYSICAL_OFFSET: usize = 0xFFFF_FFC0_0000_0000;
pub const KERNEL_PHYS_OFFSET: usize = 0x8020_0000;

#[doc(hidden)]
pub static PHYSICAL_OFFSET: AtomicUsize = AtomicUsize::new(0);

fn physical_offset() -> usize {
    PHYSICAL_OFFSET.load(Ordering::Relaxed)
}

/// Direct-mapped addresses are physical addresses that are mapped at a fixed
/// offset. Before paging is set up, this is the same as [`Identity`]. However,
/// once paging is set up, it maps starting at [`ACTUAL_PHYSICAL_OFFSET`] (i.e.
/// `0xFFFF_FFC0_0000_0000`). This way, you can store a [`DirectMapped`]
/// [`Physical`] address and convert it to a [`Virtual`] one for use before
/// paging is set up without problems. See [the caution section](#caution)
///
/// This is only valid for the first 64 GiB of physical memory. This may be
/// increased in the future.
///
/// # **Caution**
///
/// **Do not store a [`DirectMapped`] [`Virtual`] address past the paging
/// boundary**. If you do so, this pointer will become **immediately
/// invalidated** once paging is enabled. Instead, if you must store a pointer
/// across the paging boundary, store it as a [`DirectMapped`] [`Physical`]
/// pointer and convert when accessing it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirectMapped;

/// Kernel-mapped addresses are addresses that are part of the kernel code.
/// These are mapped at a fixed offset, however they are mapped such that
/// different parts of the kernel have different privileges (e.g. `.text` has
/// execute permissions, but `.data` does not). If you need to store a code
/// address or a pointer to a static, use this mapping type.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kernel;

/// Identity-mapped addresses do not have any mapping applied. Use with severe
/// caution, as it is likely they may not be valid. However, conversions are
/// cheap and always safe--the `unchecked` conversion functions will never
/// cause UB for [`Identity`] mapped addresses.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identity;

pub trait Mapping: super::_private::Sealed + Default + fmt::Debug
where
    Self: Sized,
{
    /// The virtual address space of this [`Mapping`].
    fn vaddr_space() -> Range<usize>;
    /// The physical address space of this [`Mapping`].
    fn paddr_space() -> Range<usize>;

    /// Convert a [`Physical`] address using this [`Mapping`] to a [`Virtual`]
    /// address.
    ///
    /// This function will return `None` if the physical address was not within
    /// the [`Mapping::paddr_space`].
    fn phys2virt<T, M: Mutability<T>>(_: Physical<T, Self, M>) -> Option<Virtual<T, Self, M>>;

    /// Convert a [`Physical`] address using this [`Mapping`] to a [`Virtual`]
    /// address.
    ///
    /// # Safety
    ///
    /// The caller of this function must guarantee that the physical address is
    /// within the [`Mapping::paddr_space`].
    unsafe fn phys2virt_unchecked<T, M: Mutability<T>>(
        _: Physical<T, Self, M>,
    ) -> Virtual<T, Self, M>;

    /// Convert a [`Virtual`] address using this [`Mapping`] to a [`Physical`]
    /// address.
    ///
    /// This function will return `None` if the physical address was not within
    /// the [`Mapping::vaddr_space`].
    fn virt2phys<T, M: Mutability<T>>(_: Virtual<T, Self, M>) -> Option<Physical<T, Self, M>>;

    /// Convert a [`Virtual`] address using this [`Mapping`] to a [`Physical`]
    /// address.
    ///
    /// # Safety
    ///
    /// The caller of this function must guarantee that the virtual address is
    /// within the [`Mapping::vaddr_space`].
    unsafe fn virt2phys_unchecked<T, M: Mutability<T>>(
        _: Virtual<T, Self, M>,
    ) -> Physical<T, Self, M>;
}

impl Mapping for DirectMapped {
    fn vaddr_space() -> Range<usize> {
        let poff = physical_offset();
        (poff + 0.gib())..(poff + 64.gib())
    }

    fn paddr_space() -> Range<usize> {
        0.gib()..64.gib()
    }

    fn phys2virt<T, M: Mutability<T>>(paddr: Physical<T, Self, M>) -> Option<Virtual<T, Self, M>> {
        let poff = physical_offset();
        let paddr = paddr.into_usize();
        let vaddr = paddr.checked_add(poff)?;
        Virtual::try_from_usize(vaddr)
    }

    #[inline]
    unsafe fn phys2virt_unchecked<T, M: Mutability<T>>(
        paddr: Physical<T, Self, M>,
    ) -> Virtual<T, Self, M> {
        let poff = physical_offset();
        let paddr = paddr.into_usize();
        let vaddr = paddr + poff;
        unsafe { Virtual::from_usize_unchecked(vaddr) }
    }

    fn virt2phys<T, M: Mutability<T>>(vaddr: Virtual<T, Self, M>) -> Option<Physical<T, Self, M>> {
        let poff = physical_offset();
        let vaddr = vaddr.into_usize();
        let paddr = vaddr.checked_sub(poff)?;
        Physical::try_from_usize(paddr)
    }

    #[inline]
    unsafe fn virt2phys_unchecked<T, M: Mutability<T>>(
        vaddr: Virtual<T, Self, M>,
    ) -> Physical<T, Self, M> {
        let poff = physical_offset();
        let vaddr = vaddr.into_usize();
        let paddr = vaddr - poff;
        unsafe { Physical::from_usize_unchecked(paddr) }
    }
}

impl Mapping for Kernel {
    fn vaddr_space() -> Range<usize> {
        let kernel_size = kernel_end().into_usize() - kernel_start().into_usize() + 4.kib();
        KERNEL_OFFSET..(KERNEL_OFFSET + kernel_size)
    }

    fn paddr_space() -> Range<usize> {
        let kernel_size = kernel_end().into_usize() - kernel_start().into_usize() + 4.kib();
        KERNEL_PHYS_OFFSET..(KERNEL_PHYS_OFFSET + kernel_size)
    }

    fn phys2virt<T, M: Mutability<T>>(paddr: Physical<T, Self, M>) -> Option<Virtual<T, Self, M>> {
        let paddr = paddr.into_usize();
        let vaddr = paddr
            .checked_add(KERNEL_OFFSET)?
            .checked_sub(KERNEL_PHYS_OFFSET)?;
        Virtual::try_from_usize(vaddr)
    }

    #[inline]
    unsafe fn phys2virt_unchecked<T, M: Mutability<T>>(
        paddr: Physical<T, Self, M>,
    ) -> Virtual<T, Self, M> {
        let paddr = paddr.into_usize();
        let vaddr = paddr + KERNEL_OFFSET - KERNEL_PHYS_OFFSET;
        unsafe { Virtual::from_usize_unchecked(vaddr) }
    }

    fn virt2phys<T, M: Mutability<T>>(vaddr: Virtual<T, Self, M>) -> Option<Physical<T, Self, M>> {
        let vaddr = vaddr.into_usize();
        let paddr = vaddr
            .checked_sub(KERNEL_OFFSET)?
            .checked_add(KERNEL_PHYS_OFFSET)?;
        Physical::try_from_usize(paddr)
    }

    #[inline]
    unsafe fn virt2phys_unchecked<T, M: Mutability<T>>(
        vaddr: Virtual<T, Self, M>,
    ) -> Physical<T, Self, M> {
        let vaddr = vaddr.into_usize();
        let paddr = vaddr - KERNEL_OFFSET + KERNEL_PHYS_OFFSET;
        unsafe { Physical::from_usize_unchecked(paddr) }
    }
}

impl Mapping for Identity {
    fn vaddr_space() -> Range<usize> {
        0..usize::MAX
    }

    fn paddr_space() -> Range<usize> {
        0..usize::MAX
    }

    #[inline(always)]
    fn phys2virt<T, M: Mutability<T>>(paddr: Physical<T, Self, M>) -> Option<Virtual<T, Self, M>> {
        // SAFETY: All address ranges are valid for the identity mapping
        Some(unsafe { Virtual::from_usize_unchecked(paddr.into_usize()) })
    }

    #[inline(always)]
    fn virt2phys<T, M: Mutability<T>>(vaddr: Virtual<T, Self, M>) -> Option<Physical<T, Self, M>> {
        // SAFETY: All address ranges are valid for the identity mapping
        Some(unsafe { Physical::from_usize_unchecked(vaddr.into_usize()) })
    }

    #[inline(always)]
    unsafe fn virt2phys_unchecked<T, M: Mutability<T>>(
        vaddr: Virtual<T, Self, M>,
    ) -> Physical<T, Self, M> {
        unsafe { Self::virt2phys(vaddr).unwrap_unchecked() }
    }

    unsafe fn phys2virt_unchecked<T, M: Mutability<T>>(
        paddr: Physical<T, Self, M>,
    ) -> Virtual<T, Self, M> {
        unsafe { Self::phys2virt(paddr).unwrap_unchecked() }
    }
}
