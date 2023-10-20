//! This module defines the capability table interface used within all
//! syscalls in river.
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

use core::{marker::PhantomData, num::NonZeroU16};

pub trait Capability {}

/// A `Captr`, or capability pointer is an opaque, unforgeable
/// token[^1] to a resource in the kernel, represented internally as
/// an `Option<NonZeroU16>`. It can not be "dereferenced" but can be
/// passed to syscalls to perform operations on kernel resources. It
/// is `!Send` and `!Sync` as threads spawned from some parent thread
/// may have similar capability tables but the indices are likely not
/// stable, and the child thread may not have the same capability
/// access as the parent thread.
///
/// [^1]: For more information on what this means, see the
/// [module-level documentation][self].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Captr<C: Capability> {
    inner: Option<NonZeroU16>,
    /// N.B. we must be !Send + !Sync as threads we spawn have
    /// different (but likely related) captbls. Owning a *const _
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
    pub unsafe fn from_raw_unchecked(inner: u16) -> Self {
        Self {
            inner: NonZeroU16::new(inner),
            _marker: PhantomData,
        }
    }

    /// Convert a `Captr` into its raw representation.
    #[must_use]
    pub fn into_raw(self) -> u16 {
        self.inner.map(NonZeroU16::get).unwrap_or_default()
    }

    /// Check if a `Captr` is null.
    pub fn is_null(self) -> bool {
        self.inner.is_none()
    }

    /// Create a null `Captr`.
    pub fn null() -> Self {
        Self {
            inner: None,
            _marker: PhantomData,
        }
    }
}

/// A `RemoteCaptr` is a capability pointer referenced to a child
/// capability table of the current process, i.e. it consists of two
/// capability pointers, one pointing to the [`Captbl`] it references
/// from, and one indexed within that capability table.
///
/// When the reference table pointer is null, it refers to the root
/// capability table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct RemoteCaptr<C: Capability> {
    subtable: Captr<Captbl>,
    index: Captr<C>,
}

impl<C: Capability> RemoteCaptr<C> {}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Captbl {}

impl Capability for Captbl {}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Empty {}

impl Capability for Empty {}

mod syscall_drafts {

    // not actual type signatures -- ffi boundary details to be worked
    // out later

    use super::*;

    struct Error;
    type Result<T> = core::result::Result<T, Error>;

    mod captbl {
        use super::*;
        fn copy<C: Capability>(from: RemoteCaptr<C>, into: RemoteCaptr<Empty>) -> Result<Captr<C>> {
            todo!()
        }
    }
}
