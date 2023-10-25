//! Utilities for translating virtual and physical addresses using
//! invertible mappings (i.e. not page tables).
//!
//! These utilities can be helpful when dealing with page tables in
//! userspace, however the invertible mapping portion is not as
//! useful--you will almost always want to use `Identity`, but I have
//! opened this up for further development in case I find a proper
//! usage for invertible non-identity mappings in userspace.
// TODO: document address format here? - canonicalization

// spurious trigger
#![allow(clippy::derive_partial_eq_without_eq)]
use core::fmt;

mod impls;
mod mappings;
mod phys;
mod virt;

#[cfg(any(doc, feature = "kernel"))]
pub use mappings::{
    DirectMapped, Kernel, ACTUAL_PHYSICAL_OFFSET, KERNEL_OFFSET, KERNEL_PHYS_OFFSET,
    PHYSICAL_OFFSET,
};
pub use mappings::{Identity, Mapping};
pub use phys::{Physical, Ppn};
pub use virt::{Virtual, Vpn, Vpns};

/// A virtual `*const T`, with the given mapping.
pub type VirtualConst<T, Map> = Virtual<T, Map, Const>;
/// A virtual `*mut T`, with the given mapping.
pub type VirtualMut<T, Map> = Virtual<T, Map, Mut>;
/// A physical `*const T`, with the given mapping.
pub type PhysicalConst<T, Map> = Physical<T, Map, Const>;
/// A physical `*mut T`, with the given mapping.
pub type PhysicalMut<T, Map> = Physical<T, Map, Mut>;

/// Type tag for `*const T`-alike pointers.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Const;
/// Type tag for `*mut T`-alike pointers.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mut;

/// Trait for type tags for pointer mutability. You should not need to
/// worry about using this, unless you're doing weird type stuff.
pub trait Mutability: _private::Sealed + Default + fmt::Debug {
    /// What type of pointer is T, with this mutability?
    type RawPointer<T>: fmt::Pointer + Copy + Clone;

    /// Convert [`Self::RawPointer`] to a [`usize`].
    fn into_usize<T>(x: Self::RawPointer<T>) -> usize;
}

impl Mutability for Const {
    type RawPointer<T> = *const T;

    fn into_usize<T>(x: Self::RawPointer<T>) -> usize {
        x.cast::<u8>() as usize
    }
}

impl Mutability for Mut {
    type RawPointer<T> = *mut T;

    fn into_usize<T>(x: Self::RawPointer<T>) -> usize {
        x.cast::<u8>() as usize
    }
}

mod _private {
    use super::{Const, Mut};

    pub trait Sealed {}
    impl Sealed for Const {}
    impl Sealed for Mut {}
}

/// A 12-bit offset into a 4KiB (`4096` bytes) page. See [the
/// module-level documentation](crate::addr).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PgOff(u16);

/// A 12-bit mask.
const PGOFF_MASK: usize = 0b1111_1111_1111;

impl PgOff {
    /// Convert a page offset from a [`usize`], truncating extraneous
    /// bits if necessary.
    pub fn from_usize_truncate(pgoff: usize) -> Self {
        Self((pgoff & PGOFF_MASK) as u16)
    }

    /// Convert a page offset into a [`usize`].
    pub fn into_usize(self) -> usize {
        self.0 as usize
    }

    /// Convert a page offset into a [`u16`].
    pub fn into_u16(self) -> u16 {
        self.0
    }
}
