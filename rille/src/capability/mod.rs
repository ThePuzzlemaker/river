//! Capability table interface for use in all syscalls in river.
//!
//! # A Rough Overview
//!
//! River is a capability-based operating system. Thus, all syscalls
//! require certain "capabilities" to perform, and the set of which
//! capabilities your thread has access to determines what actions it
//! can perform.
//!
//! To put it briefly, a capability is an unforgeable token. This
//! means that it refers to some resource in the kernel (e.g., a page
//! table, a thread, an IPC endpoint, a device, etc.) that a given
//! thread can perform certain actions on. Unforgeable here means
//! simply that there is no way for a thread to "fake" having a
//! capability--i.e., if they have the capability, they are guaranteed
//! to have "proper" access to the resource.
//!
//! Interestingly enough, a *nix file descriptor is a somewhat good
//! example of the (rough) idea of a capability: it is an opaque,
//! unforgeable value given to the userspace thread by the kernel (via
//! `open`) that provides access to a resource (`read`, `write`,
//! etc.).
//!
//! Capablities in river are represented to the userspace thread as a
//! [`Captr`] ("capability pointer", pronounced like "captor"; but to
//! be frank, pronounce it however you like). This is similar to a
//! *nix file descriptor in that it is simply a number (represented by
//! a `usize`) that is an index in a thread's capability
//! table. Userspace threads cannot "dereference" a [`Captr`], but
//! they can enumerate details about what it points to (i.e., what
//! type of capability, and any relevant metadata as necessary).

use core::{
    cmp,
    convert::Infallible,
    fmt::Debug,
    hash::{self, Hash},
    marker::PhantomData,
    num::NonZeroUsize,
};

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::syscalls;

pub mod paging;

/// This trait is implemented by all of the opaque types representing
/// objects in the kernel. [`Captr`]s have a generic type parameter
/// `C: Capability` to ensure type-safety of typical `Captr` usage.
pub trait Capability: Copy + Clone + Debug + private::Sealed {
    /// A value, with a maximum size of `u64` by convention, that
    /// provides size information for dynamically-sized kernel objects
    /// when retyping.
    type RetypeSizeSpec;

    /// Convert a [`Self::RetypeSizeSpec`] into a [`usize`]. Because
    /// of some funky type system stuff we don't just put an
    /// [`Into<usize>`] bound on `RetypeSizeSpec`.
    fn retype_size_spec(spec: Self::RetypeSizeSpec) -> usize;

    /// What [`CapabilityType`] does this `Capability` correspond to?
    const CAPABILITY_TYPE: CapabilityType;
}

/// This enum describes the various types of capabilities that can
/// exist. It is internally represented in the kernel as a 5-bit
/// value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum CapabilityType {
    /// The [`Empty`] capability.
    Empty = 0,
    /// Capability tables, or [`Captbl`]s.
    Captbl = 1,
    // /// [`Untyped`] memory blocks.
    // Untyped = 2,
    /// TODO
    Allocator = 2,
    /// [`PageTable`][paging::PageTable]s.
    PgTbl = 3,
    /// [`Page`][paging::Page]s that can be mapped into
    /// [`PageTable`][paging::PageTable]s.
    Page = 4,
    /// Any capability type `>=32` is undefined, as the `cap_type`
    /// field in all capability slots only occupies 5 bits (`0` to
    /// `31`).
    #[num_enum(default)]
    Unknown = 32,
}

#[allow(clippy::derivable_impls)]
impl Default for CapabilityType {
    fn default() -> Self {
        CapabilityType::Empty
    }
}

impl From<u64> for CapabilityType {
    fn from(value: u64) -> Self {
        (value as u8).into()
    }
}

impl From<CapabilityType> for u64 {
    fn from(value: CapabilityType) -> Self {
        u8::from(value) as u64
    }
}

/// This enum defines some errors that may arise from capability
/// interface syscalls.
#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum CapError {
    /// This is not an error.
    NoError = 0,

    /// The requested resource was not present.
    NotPresent = 1,

    /// The requested resource was of an invalid type for this
    /// operation.
    InvalidType = 2,

    /// Not enough memory was available in an [`Untyped`] or
    /// [`Captbl`] capability to hold a given resource.
    NotEnoughResources = 3,

    /// The provided size was invalid.
    InvalidSize = 4,

    /// The requested operation could not be performed.
    InvalidOperation = 5,

    /// A capability could not be derived, as it had children.
    RevokeFirst = 6,

    /// The error provided to us by the syscall interface was unknown.
    #[num_enum(default)]
    UnknownError = u64::MAX,
}

/// Helper [`Result`] type for operations involving capability
/// syscalls.
pub type CapResult<T> = Result<T, CapError>;

/// A `Captr`, or capability pointer is an opaque, unforgeable
/// token[^1] to a resource in the kernel, represented internally as
/// an `Option<NonZeroUsize>`. It can not be "dereferenced" but can be
/// passed to syscalls to perform operations on kernel resources. It
/// is `!Send` and `!Sync` as threads spawned from some parent thread
/// may have similar capability tables but the indices are likely not
/// stable, and the child thread may not have the same capability
/// access as the parent thread.
///
/// [^1]: For more information on what this means, see the
/// [module-level documentation][self].
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct Captr<C: Capability> {
    inner: Option<NonZeroUsize>,
    /// N.B. we must be `!Send` + `!Sync` as threads we spawn have
    /// different (but likely related) captbls. Owning a `*const _`
    /// makes this true. `C` is so the type param is not unused.
    _marker: PhantomData<*const C>,
}

impl<C: Capability> Captr<C> {
    /// Create a `Captr` from a given raw capability pointer,
    /// without checking if it is valid or of the proper capability
    /// type.
    ///
    /// # Safety
    ///
    /// The capability must be valid and of the type indicated by the
    /// type parameter `C`.
    #[inline]
    pub const unsafe fn from_raw_unchecked(inner: usize) -> Self {
        Self {
            inner: NonZeroUsize::new(inner),
            _marker: PhantomData,
        }
    }

    /// Convert a `Captr` into its raw representation.
    #[must_use]
    #[inline]
    pub fn into_raw(self) -> usize {
        self.inner.map(NonZeroUsize::get).unwrap_or_default()
    }

    #[inline]
    /// Check if a `Captr` is null.
    pub const fn is_null(self) -> bool {
        self.inner.is_none()
    }

    #[inline]
    /// Create a null `Captr`.
    pub const fn null() -> Self {
        Self {
            inner: None,
            _marker: PhantomData,
        }
    }

    /// Offset a `Captr` by an [`isize`]. Note that this function may
    /// overflow, and may create a null `Captr`.
    #[inline]
    #[must_use]
    pub fn offset(self, by: isize) -> Self {
        let inner = self
            .inner
            .and_then(|x| NonZeroUsize::new(x.get().wrapping_add_signed(by)));
        Self { inner, ..self }
    }

    /// Offset a `Captr` by a [`usize`]. Note that this function may
    /// overflow, and may create a null `Captr`.
    #[inline]
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, by: usize) -> Self {
        let inner = self
            .inner
            .and_then(|x| NonZeroUsize::new(x.get().wrapping_add(by)));
        Self { inner, ..self }
    }
}

/// A `RemoteCaptr` is a capability pointer referenced to a child
/// capability table of the current process, i.e. it consists of two
/// capability pointers, one pointing to the [`Captbl`] it references
/// from, and one indexed within that capability table.
///
/// When the reference table pointer is null, it refers to the root
/// capability table of the current thread.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct RemoteCaptr<C: Capability> {
    reftbl: Captr<Captbl>,
    index: Captr<C>,
}

impl<C: Capability> RemoteCaptr<C> {
    /// Create a `RemoteCaptr` that refers to a capability within the
    /// provided reference captbl (`reftbl`).
    #[inline]
    pub const fn remote(reftbl: Captr<Captbl>, index: Captr<C>) -> RemoteCaptr<C> {
        Self { reftbl, index }
    }

    /// Create a `RemoteCaptr` that refers to a capability local to
    /// the current thread's root capability table.
    #[inline]
    pub const fn local(index: Captr<C>) -> RemoteCaptr<C> {
        Self::remote(Captr::null(), index)
    }

    /// Returns true if this `RemoteCaptr` is local, i.e. the
    /// reference table `Captr` is null.
    #[inline]
    pub const fn is_local(self) -> bool {
        self.reftbl.is_null()
    }

    /// Returns the local index of this `RemoteCaptr`.
    #[inline]
    pub const fn local_index(self) -> Captr<C> {
        self.index
    }

    /// Returns the reference table index of this `RemoteCaptr`.
    pub const fn reftbl(self) -> Captr<Captbl> {
        self.reftbl
    }
}

/// A capability table corresponds to the kernel's map from [`Captr`]
/// indices to capability objects in the kernel. Additional metadata
/// is associated with capabilities in the kernel as needed; most of
/// which is not accessible to userspace code as it is not relevant
/// there.
///
/// Captbls have a certain number of "slots" of [`Empty`]
/// capabilities, where capabilities can be stored. By convention, the
/// "null capability" (index 0) is always empty and it is prohibited
/// to store a capability there. This is so the representation of an
/// `Option<Captr<_>>` can be the same size as a `Captr<_>`.
///
/// Each slot in the capability table is 32 bytes wide. When
/// calculating the size, the null capability is included as it is
/// used internally in the kernel to store additional metadata about
/// the captbl overall.
#[derive(Copy, Clone, Debug)]
pub struct Captbl(#[doc(hidden)] Infallible);

impl Capability for Captbl {
    type RetypeSizeSpec = CaptblSizeSpec;

    fn retype_size_spec(spec: Self::RetypeSizeSpec) -> usize {
        spec.n_slots_log2
    }

    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Captbl;
}

/// Options for retyping an [`Untyped`] capability into a [`Captbl`]
/// capability.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct CaptblSizeSpec {
    /// The base-2 logarithm of the number of slots of the new
    /// [`Captbl`]. The overall byte size of the captbl is given as
    /// follows: `2.pow(n_slots_log2) * 32`.
    pub n_slots_log2: usize,
}

/// The empty capability corresponds to an empty slot in the
/// capability table, into which any capability can be placed.
#[derive(Copy, Clone, Debug)]
pub struct Empty(#[doc(hidden)] Infallible);

impl Capability for Empty {
    /// Untyped memory cannot be retyped to Empty capabilities.
    type RetypeSizeSpec = Infallible;

    fn retype_size_spec(_: Self::RetypeSizeSpec) -> usize {
        0
    }

    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Empty;
}

// /// The untyped capability corresponds to a block of kernel memory
// /// that can be allocated for many purposes; for example, [virtual
// /// memory pages][paging::Page], [capability tables][Captbl], or even
// /// smaller untyped capabilities.
// ///
// /// For more information on retyping, see [`Captr<Untyped>::retype`].
// #[derive(Copy, Clone, Debug)]
// pub struct Untyped(#[doc(hidden)] Infallible);

// impl Capability for Untyped {
//     type RetypeSizeSpec = UntypedSizeSpec;

//     fn retype_size_spec(spec: Self::RetypeSizeSpec) -> usize {
//         spec.n_bytes_log2
//     }

//     const CAPABILITY_TYPE: CapabilityType = CapabilityType::Untyped;
// }

/// Options for retyping an [`Untyped`] capability into a child
/// `Untyped` capability.
#[derive(Copy, Clone, Debug)]
#[repr(transparent)]
pub struct UntypedSizeSpec {
    /// The base-2 logarithm of the byte size of the new [`Untyped`]
    /// capability. This must be less than or equal to the size of the
    /// parent untyped capability.
    pub n_bytes_log2: usize,
}

impl RemoteCaptr<Captbl> {
    /// Copy a capability in a potentially remote [`Captbl`] into
    /// another slot in another potentially remote [`Captbl`].
    ///
    /// Note that `self` and `into` can refer to the same [`Captbl`].
    ///    
    /// # Errors
    ///
    /// If any capability was not present and was required,
    /// [`CapError::NotPresent`] will be returned. If any capability
    /// was of an invalid type, [`CapError::InvalidType`] is returned.
    pub fn copy_deep<C: Capability>(
        self,
        from_index: Captr<C>,
        into: RemoteCaptr<Captbl>,
        into_index: Captr<Empty>,
    ) -> CapResult<Captr<C>> {
        syscalls::captbl::copy_deep(
            self.reftbl().into_raw(),
            self.local_index().into_raw(),
            from_index.into_raw(),
            into.reftbl().into_raw(),
            into.local_index().into_raw(),
            into_index.into_raw(),
        )?;

        // SAFETY: By the invariants of the copy_deep syscall, this is valid.
        Ok(unsafe { Captr::from_raw_unchecked(into_index.into_raw()) })
    }
}

impl<C: Capability> RemoteCaptr<C> {
    /// Split a `RemoteCaptr<C>` into a local `RemoteCaptr<Captbl>`
    /// (from the `reftbl`) and a `Captr<C>`.
    #[inline]
    #[must_use]
    pub const fn split(self) -> (RemoteCaptr<Captbl>, Captr<C>) {
        (RemoteCaptr::local(self.reftbl), self.index)
    }

    /// Copy the capability referred to by this `RemoteCaptr` into
    /// another empty slot, returning the new `RemoteCaptr`.
    ///
    /// # Errors
    ///
    /// If any capability was not present and was required,
    /// [`CapError::NotPresent`] will be returned. If any capability
    /// was of an invalid type, [`CapError::InvalidType`] is returned.
    pub fn copy(self, into: RemoteCaptr<Empty>) -> CapResult<RemoteCaptr<C>> {
        let (from, from_index) = self.split();
        let (into, into_index) = into.split();
        let res = from.copy_deep(from_index, into, into_index)?;
        Ok(RemoteCaptr::remote(from.local_index(), res))
    }

    /// Remove the capability from the slot referred to by this
    /// `RemoteCaptr`. Children capabilities derived from it are not
    /// affected.
    ///
    /// If this was the last slot referencing the capability, the
    /// underlying object will be deleted.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn delete(self) -> CapResult<RemoteCaptr<Empty>> {
        syscalls::captbl::delete(self.reftbl().into_raw(), self.local_index().into_raw())?;

        // SAFETY: We have just deleted the capability and succeeded,
        // therefore it is empty.
        Ok(RemoteCaptr::remote(self.reftbl(), unsafe {
            Captr::from_raw_unchecked(self.local_index().into_raw())
        }))
    }

    /// Atomically swap the capabilities referred to by two
    /// `RemoteCaptr`s, potentially of different types.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn swap<C2: Capability>(
        self,
        other: RemoteCaptr<C2>,
    ) -> CapResult<(RemoteCaptr<C2>, RemoteCaptr<C>)> {
        syscalls::captbl::swap(
            self.reftbl().into_raw(),
            self.local_index().into_raw(),
            other.reftbl().into_raw(),
            other.local_index().into_raw(),
        )?;

        Ok((
            RemoteCaptr::remote(
                self.reftbl,
                Captr {
                    inner: self.index.inner,
                    _marker: PhantomData,
                },
            ),
            RemoteCaptr::remote(
                self.reftbl,
                Captr {
                    inner: self.index.inner,
                    _marker: PhantomData,
                },
            ),
        ))
    }
}

// impl Captr<Untyped> {
//     /// Allocate 1 or more capabilities from an [`Untyped`] capability.
//     ///
//     /// This will allocate `count` capabilities from this [`Untyped`]
//     /// capability, putting the resulting capabilities into the slots
//     /// starting at `into[starting_at]` to, but not including
//     /// `into[starting_at + count]`. Note that all these slots within
//     /// this range must be [`Empty`] capabilities.
//     ///
//     /// The resultant iterator provides [`Captr<C>`]'s over the range
//     /// `starting_at..(starting_at + count)` from the empty
//     /// `Captr<Empty>` range provided to the function.
//     ///
//     /// `size` and [`Capability::RetypeSizeSpec`] are used to
//     /// determine the size of dynamic objects. See the capability's
//     /// documentation for more information.
//     ///
//     /// # Errors
//     ///
//     /// TODO
//     pub fn retype_many<C: Capability>(
//         self,
//         into: RemoteCaptr<Captbl>,
//         starting_at: Captr<Empty>,
//         count: usize,
//         size: C::RetypeSizeSpec,
//     ) -> CapResult<RetypeIter<C>> {
//         syscalls::untyped::retype_many(
//             self.into_raw(),
//             into.reftbl().into_raw(),
//             into.local_index().into_raw(),
//             starting_at.into_raw(),
//             count,
//             C::CAPABILITY_TYPE,
//             C::retype_size_spec(size),
//         )?;

//         // SAFETY: We know that starting_at points to a valid
//         // capability due to the invariants of the retype_many
//         // syscall.
//         let starting_at = unsafe { Captr::from_raw_unchecked(starting_at.into_raw()) };

//         Ok(RetypeIter {
//             starting_at,
//             end: starting_at.add(count),
//         })
//     }

//     /// Allocate a capability from an [`Untyped`] capability.
//     ///
//     /// This will allocate 1 capability from this [`Untyped`]
//     /// capability, putting the resulting capabilities into the slot
//     /// indexed by `into[at]`.
//     ///
//     /// The resultant [`Captr<C>`] is simply a casted version of `at`,
//     /// with the capability now ensured to be present.
//     ///
//     /// `size` and [`Capability::RetypeSizeSpec`] are used to
//     /// determine the size of dynamic objects. See the capability's
//     /// documentation for more information.
//     ///
//     /// # Errors
//     ///
//     /// TODO
//     pub fn retype<C: Capability>(
//         self,
//         into: RemoteCaptr<Captbl>,
//         at: Captr<Empty>,
//         size: C::RetypeSizeSpec,
//     ) -> CapResult<Captr<C>> {
//         syscalls::untyped::retype_many(
//             self.into_raw(),
//             into.reftbl().into_raw(),
//             into.local_index().into_raw(),
//             at.into_raw(),
//             1,
//             C::CAPABILITY_TYPE,
//             C::retype_size_spec(size),
//         )?;

//         // SAFETY: By the invariants of the retype_many syscall, this
//         // is valid.
//         let at = unsafe { Captr::from_raw_unchecked(at.into_raw()) };
//         Ok(at)
//     }
// }

/// An iterator that provides the resulting [`Captr`]s from a
/// [`Captr::<Untyped>::retype_many`] invocation.
#[derive(Clone, Debug)]
pub struct RetypeIter<C: Capability> {
    starting_at: Captr<C>,
    end: Captr<C>,
}

impl<C: Capability> Iterator for RetypeIter<C> {
    type Item = Captr<C>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.starting_at >= self.end {
            None
        } else {
            let cap = self.starting_at;
            self.starting_at = self.starting_at.add(1);
            Some(cap)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end.into_raw() - self.starting_at.into_raw();
        (n, Some(n))
    }
}

impl<C: Capability> ExactSizeIterator for RetypeIter<C> {
    fn len(&self) -> usize {
        self.end.into_raw() - self.starting_at.into_raw()
    }
}

// fn captbl_alloc(captbl: Captr<Captbl>, n: usize) -> CapResult<Captr<Empty>> {
//     todo!()
// }

// const ROOT_CAPTBL: Captr<Captbl> = unsafe { Captr::from_raw_unchecked(1) };

// fn test(ut: Captr<Untyped>) -> CapResult<()> {
//     use paging::{BasePage, GigaPage, MegaPage, Page, PageTable, PageTableFlags};
//     let free_nodes_start = captbl_alloc(ROOT_CAPTBL, 4)?;
//     let mut iter = ut.retype_many(RemoteCaptr::local(ROOT_CAPTBL), free_nodes_start, 3, ())?;
//     let pgtbl_l2: Captr<PageTable<BasePage>> = iter.next().unwrap().into();
//     let pgtbl_l1: Captr<PageTable<MegaPage>> = iter.next().unwrap().into();
//     let pgtbl_l0: Captr<PageTable<GigaPage>> = iter.next().unwrap();

//     let page = ut.retype::<Page<BasePage>>(
//         RemoteCaptr::local(ROOT_CAPTBL),
//         free_nodes_start.offset(3),
//         (),
//     )?;

//     page.map(
//         pgtbl_l2,
//         Vpn((0x10000usize >> 12) as u16),
//         PageTableFlags::RW,
//     )?;
//     pgtbl_l2.map(pgtbl_l1, Vpn(0), PageTableFlags::RW)?;
//     pgtbl_l1.map(pgtbl_l0, Vpn(0), PageTableFlags::RW)?;

//     Ok(())
// }

/// An inclusive range of capability pointers.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct CaptrRange<C: Capability> {
    /// The low end of the range.
    pub lo: Captr<C>,
    /// The high end of the range, inclusive.
    pub hi: Captr<C>,
}

impl<C: Capability> CaptrRange<C> {
    pub fn count(&self) -> usize {
        (self.hi.into_raw() - self.lo.into_raw()) + 1
    }
}

// == Unimportant or boilerplate-y impls below ==

mod private {
    use super::{
        paging::{BasePage, GigaPage, MegaPage, Page, PageTable, PagingLevel},
        Captbl,
        Empty, //Untyped,
    };

    pub trait Sealed {}
    impl Sealed for GigaPage {}
    impl Sealed for MegaPage {}
    impl Sealed for BasePage {}
    impl<L: PagingLevel> Sealed for Page<L> {}
    impl<L: PagingLevel> Sealed for PageTable<L> {}
    impl Sealed for Captbl {}
    impl Sealed for Empty {}
    //impl Sealed for Untyped {}
}

// These impls are just so Capability doesn't have to impl all these
// traits, since that's just kinda annoying (clutters up all the trait
// bounds in the docs, and since you can't construct all
// `Capability`'s, it doesn't matter)

impl<C: Capability> PartialOrd for Captr<C> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C: Capability> PartialEq for Captr<C> {
    fn eq(&self, other: &Self) -> bool {
        self.inner.eq(&other.inner)
    }
}

impl<C: Capability> Eq for Captr<C> {}

impl<C: Capability> Ord for Captr<C> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl<C: Capability> Hash for Captr<C> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl<C: Capability> PartialEq for RemoteCaptr<C> {
    fn eq(&self, other: &Self) -> bool {
        self.reftbl.eq(&other.reftbl) && self.index.eq(&other.index)
    }
}

impl<C: Capability> Eq for RemoteCaptr<C> {}

impl<C: Capability> Hash for RemoteCaptr<C> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.reftbl.hash(state);
        self.index.hash(state);
    }
}
