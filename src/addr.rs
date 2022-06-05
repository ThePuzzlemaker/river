// spurious trigger
#![allow(clippy::derive_partial_eq_without_eq)]
use core::fmt;

mod impls;
mod mappings;
mod phys;
mod virt;

pub use mappings::{
    DirectMapped, Identity, Kernel, Mapping, ACTUAL_PHYSICAL_OFFSET, KERNEL_OFFSET,
    KERNEL_PHYS_OFFSET, PHYSICAL_OFFSET,
};
pub use phys::{Physical, Ppn};
pub use virt::{Virtual, Vpn};

pub type VirtualConst<T, Map> = Virtual<T, Map, Const>;
pub type VirtualMut<T, Map> = Virtual<T, Map, Mut>;
pub type PhysicalConst<T, Map> = Physical<T, Map, Const>;
pub type PhysicalMut<T, Map> = Physical<T, Map, Mut>;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Const;
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mut;

pub trait Mutability<T>: _private::Sealed + Default + fmt::Debug {
    type RawPointer: fmt::Pointer + Copy + Clone;

    fn into_usize(ptr: Self::RawPointer) -> usize;
}

impl<T> Mutability<T> for Const {
    type RawPointer = *const T;

    #[inline(always)]
    fn into_usize(ptr: Self::RawPointer) -> usize {
        ptr as usize
    }
}

impl<T> Mutability<T> for Mut {
    type RawPointer = *mut T;

    #[inline(always)]
    fn into_usize(ptr: Self::RawPointer) -> usize {
        ptr as usize
    }
}

mod _private {
    use super::{Const, DirectMapped, Identity, Kernel, Mut};

    pub trait Sealed {}
    impl Sealed for DirectMapped {}
    impl Sealed for Kernel {}
    impl Sealed for Identity {}
    impl Sealed for Const {}
    impl Sealed for Mut {}
}

/// A 12-bit offset into a 4KiB (`4096` bytes) page.
/// See [the module-level documentation](crate::addr).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PgOff(u16);

/// A 12-bit mask.
const PGOFF_MASK: usize = 0b111111111111;

impl PgOff {
    pub fn from_usize_truncate(pgoff: usize) -> Self {
        Self((pgoff & PGOFF_MASK) as u16)
    }

    pub fn into_usize(self) -> usize {
        self.0 as usize
    }

    pub fn into_u16(self) -> u16 {
        self.0
    }
}
