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
    fmt::{self, Debug},
    hash::{self, Hash},
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
    ops::Deref,
    ptr,
};

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::{
    addr::{Identity, VirtualConst, VirtualMut},
    syscalls::{self, SyscallNumber},
};

use self::paging::{PageTable, PageTableFlags};

pub mod paging;

/// This trait is implemented by all of the opaque types representing
/// objects in the kernel.
pub trait Capability: Copy + Clone + Debug + private::Sealed + Deref<Target = Captr> {
    /// Convert a [`Captr`] to this capability type, without checking if
    /// it is of the correct type.
    fn from_captr(c: Captr) -> Self;

    /// Convert this capability into its underlying [`Captr`].
    fn into_captr(self) -> Captr {
        *self
    }

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
    /// [`PageTable`]s.
    PgTbl = 3,
    /// [`Page`][paging::Page]s that can be mapped into
    /// [`PageTable`]s.
    Page = 4,
    /// [`Thread`]s.
    Thread = 5,
    /// [`Notification`]s.
    Notification = 6,
    /// [`InterruptPool`]s.
    InterruptPool = 7,
    /// [`InterruptHandler`]s.
    InterruptHandler = 8,
    /// [`Endpoint`]s.
    Endpoint = 9,
    /// [`Job`]s.
    Job = 10,
    /// Any capability type `>=32` is undefined.
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

    /// Not enough memory was available in a [`Captbl`] capability to
    /// hold a given resource, or the system was out of memory.
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
#[derive(Copy, Clone)]
#[repr(C)]
pub struct Captr {
    inner: Option<NonZeroUsize>,
}

impl Captr {
    /// Create a `Captr` from a given raw capability pointer.
    #[inline]
    pub const fn from_raw(inner: usize) -> Self {
        Self {
            inner: NonZeroUsize::new(inner),
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
        Self { inner: None }
    }

    /// Offset a `Captr` by an [`isize`]. Note that this function may
    /// overflow, and may create a null `Captr`.
    #[inline]
    #[must_use]
    pub fn offset(self, by: isize) -> Self {
        let inner = self
            .inner
            .and_then(|x| NonZeroUsize::new(x.get().wrapping_add_signed(by)));
        Self { inner }
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
        Self { inner }
    }
}

/// The empty capability corresponds to an empty slot in the
/// capability table, into which any capability can be placed.
#[derive(Copy, Clone, Debug)]
pub struct Empty(Captr);

impl Deref for Empty {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for Empty {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Empty;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

/// Extension trait for universal captr syscalls.
pub trait CaptrOps
where
    Self: Sized,
{
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
    fn delete(self) -> CapResult<()>;

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    fn grant(self, rights: CapRights, badge: Option<NonZeroU64>) -> CapResult<Self>;
}

impl<C: Capability> CaptrOps for C {
    fn delete(self) -> CapResult<()> {
        syscalls::captr::delete(self.into_raw())?;

        Ok(())
    }

    fn grant(self, rights: CapRights, badge: Option<NonZeroU64>) -> CapResult<Self> {
        // SAFETY: CaptrGrant is always safe.
        let res = unsafe {
            syscalls::ecall3(
                SyscallNumber::CaptrGrant,
                self.into_raw() as u64,
                rights.bits,
                badge.map_or(0, NonZeroU64::get),
            )
        };

        res.map(|x| x as usize)
            .map(Captr::from_raw)
            .map(C::from_captr)
            .map_err(CapError::from)
    }
}

/// A capabiltiy representing a thread.
///
/// Threads must have a [page table][PageTable], but are not required
/// to have a [capability table][Captbl]. A thread without a captbl
/// has no ability to receive or hold capabilities.
///
/// See [`Captr<Thread>`][Captr#impl-Captr<Thread>] for operations on
/// this capability.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Thread(Captr);

impl Deref for Thread {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for Thread {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Thread;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

impl Thread {
    /// Suspend the thread referred to by this Captr.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn suspend(self) -> CapResult<()> {
        syscalls::thread::suspend(self.into_raw())
    }

    /// Resume the thread referred to by this captr.
    ///
    /// # Requirements
    ///
    /// - Threads must have a page table configured via
    ///   [`Captr::<Thread>::configure`].
    /// - The thread must be suspended.
    ///
    /// # Errors
    ///
    /// TODO
    ///
    /// # Safety
    ///
    /// When resumed, the thread must not cause undefined behaviour
    /// with respect to the current thread.
    pub unsafe fn resume(self) -> CapResult<()> {
        syscalls::thread::resume(self.into_raw())
    }

    /// Configure a thread's page table and capability table.
    ///
    /// # Requirements
    ///
    /// - The thread must be suspended.
    ///
    /// # Errors
    ///
    /// TODO
    ///
    /// # Safety
    ///
    /// The capability table and page table, when configured for the
    /// thread, must not cause the thread to perform undefined
    /// behaviour with respect to the running thread when resumed.
    pub unsafe fn configure(
        self,
        pgtbl: PageTable,
        ipc_buffer: paging::Page<paging::BasePage>,
    ) -> CapResult<()> {
        // SAFETY: By invariants.
        unsafe {
            syscalls::thread::configure(self.into_raw(), pgtbl.into_raw(), ipc_buffer.into_raw())
        }
    }

    /// Write registers of a suspended thread.
    ///
    /// # Requirements
    ///
    /// - The thread must be suspended.
    ///
    /// # Errors
    ///
    /// TODO
    ///
    /// # Safety
    ///
    /// The registers, when written to the thread, must not cause the
    /// thread to perform undefined behaviour with respect to the
    /// running thread when resumed.
    pub unsafe fn write_registers(self, registers: &UserRegisters) -> CapResult<()> {
        syscalls::thread::write_registers(self.into_raw(), registers as *const _)
    }

    /// Write initial registers to an unstarted thread, then start it.
    ///
    /// The thread will be started with its `pc` from `entry` and `sp`
    /// from `stack`.
    ///
    /// The first argument (`arg1`), if non-null, will be the `Captr`
    /// of an initial capability used to bootstrap the
    /// process. Typically this should be an [`Endpoint`] capability,
    /// to allow sending further capabilities to this process. This
    /// capability will be sent to the process and the `Captr` of the
    /// capability will be put in the `a0` register.
    ///
    /// Note that if no capability is provided, and this thread does
    /// not share a [`Job`] with any other threads, no capabilities
    /// will be able to be sent to this job.
    ///
    /// The second argument (`arg2`) will be put in the `a1` register.
    ///
    /// # Errors
    ///
    /// TODO
    ///
    /// # Safety
    ///
    /// When started with these values and in its context, this thread
    /// must not cause any undefined behaviour.
    pub unsafe fn start(
        self,
        entry: VirtualConst<u8, Identity>,
        stack: VirtualMut<u8, Identity>,
        arg1: Captr,
        arg2: u64,
    ) -> CapResult<()> {
        let res = syscalls::ecall5(
            SyscallNumber::ThreadStart,
            self.into_raw() as u64,
            entry.into(),
            stack.into(),
            arg1.into_raw() as u64,
            arg2,
        );

        if let Err(e) = res {
            Err(e.into())
        } else {
            Ok(())
        }
    }
}

/// A notification is a kernel-backed semaphore between threads, with
/// the ability to block on signals.
///
/// Notifications can also be used by the kernel for things such as
/// interrupts (WIP).
///
/// See [`Captr<Notification>`][Captr#impl-Captr<Notification>] for
/// operations on this capability.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Notification(Captr);

impl Deref for Notification {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for Notification {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Notification;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

impl Notification {
    /// Create a new notification.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn create() -> CapResult<Self> {
        // SAFETY: notification_create is always safe.
        let res = unsafe { syscalls::ecall0(SyscallNumber::NotificationCreate) };

        res.map(|x| Captr::from_raw(x as usize))
            .map(Notification::from_captr)
            .map_err(CapError::from)
    }

    /// Read the semaphore without blocking. If the semaphore was
    /// non-zero, clear it.
    ///
    /// # Errors
    ///
    /// - [`CapError::NotPresent`]: The notification capability was
    ///   not present.
    /// - [`CapError::InvalidType`]: The capability in the provided
    ///   slot was not a notification.
    pub fn poll(&self) -> CapResult<Option<NonZeroU64>> {
        syscalls::notification::poll(self.into_raw())
    }

    /// Signal this notification, bitwise `or`ing the semaphore with
    /// the capability's badge. Additionally, this operation will wake
    /// up the first thread waiting on this notification, if any.
    ///
    /// If the capability is zero-badged (or is unbadged), this
    /// operation will only wake up the first thread waiting on this
    /// notification.
    ///
    /// # Errors
    ///
    /// - [`CapError::NotPresent`]: The notification capability was
    ///   not present.
    /// - [`CapError::InvalidType`]: The capability in the provided
    ///   slot was not a notification.
    pub fn signal(&self) -> CapResult<()> {
        syscalls::notification::signal(self.into_raw())
    }

    /// Wait until another thread signals this notification, or if
    /// there are unread incoming signals, read those without
    /// waiting. Then, clear the semaphore.
    ///
    /// This function may return `Ok(None)` as it is possible that
    /// this thread is woken up by a [`Self::signal`] operation from
    /// an unbadged capability.
    ///
    /// # Errors
    ///
    /// - [`CapError::NotPresent`]: The notification capability was
    ///   not present.
    /// - [`CapError::InvalidType`]: The capability in the provided
    ///   slot was not a notification.
    pub fn wait(&self) -> CapResult<Option<NonZeroU64>> {
        syscalls::notification::wait(self.into_raw())
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
#[allow(missing_docs)]
pub struct UserRegisters {
    pub ra: u64,
    pub sp: u64,
    pub gp: u64,
    pub tp: u64,
    pub t0: u64,
    pub t1: u64,
    pub t2: u64,
    pub s0: u64,
    pub s1: u64,
    pub a0: u64,
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
    pub a7: u64,
    pub s2: u64,
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub s6: u64,
    pub s7: u64,
    pub s8: u64,
    pub s9: u64,
    pub s10: u64,
    pub s11: u64,
    pub t3: u64,
    pub t4: u64,
    pub t5: u64,
    pub t6: u64,
    pub pc: u64,
}

/// An exclusive range of capability pointers.
// TODO: IntoIterator
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct CaptrRange<C: Capability> {
    /// The low end of the range.
    pub lo: Captr,
    /// The high end of the range, exclusive.
    pub hi: Captr,
    _phantom: PhantomData<C>,
}

impl<C: Capability> CaptrRange<C> {
    /// Create a new `CaptrRange` from its bounding [`Captr`]s, without
    /// checking if the capabilities are of the valid type.
    pub fn new(lo: Captr, hi: Captr) -> Self {
        Self {
            lo,
            hi,
            _phantom: PhantomData,
        }
    }

    /// Get the low end of the `CaptrRange`.
    pub fn lo(self) -> C {
        C::from_captr(self.lo)
    }

    /// Get the high end of the `CaptrRange`. Note that this is
    /// exclusive and is thus not a valid instance of `C`.
    pub fn hi(self) -> Captr {
        self.hi
    }

    /// TODO
    pub fn count(&self) -> usize {
        (self.hi.into_raw() - self.lo.into_raw()) + 1
    }
}

bitflags::bitflags! {
    /// Rights for capabilities.
    pub struct CapRights: u64 {
    /// - [`Page`]: Can be mapped readable
    const READ = 1 << 0;
    /// - [`Page`]: Can be mapped writable
    const WRITE = 1 << 1;
    /// - [`Page`]: Can be mapped executable
    const EXECUTE = 1 << 2;
    }
}

impl Default for CapRights {
    /// All rights.
    fn default() -> Self {
        Self::all()
    }
}

impl From<u64> for CapRights {
    fn from(x: u64) -> Self {
        CapRights::from_bits_truncate(x)
    }
}

impl CapRights {
    /// Convert these rights into a mask of [`PageTableFlags`].
    pub fn into_pgtbl_mask(self) -> PageTableFlags {
        let mut x = PageTableFlags::empty();
        if self.contains(Self::READ) {
            x |= PageTableFlags::READ;
        }
        if self.contains(Self::WRITE) {
            x |= PageTableFlags::WRITE;
        }
        if self.contains(Self::EXECUTE) {
            x |= PageTableFlags::EXECUTE;
        }
        x
    }
}

/// Dynamic capabilities.
/// TODO
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AnyCap(Captr);

impl Deref for AnyCap {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for AnyCap {
    /// The capability type cannot be known at compile-time.
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Unknown;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

/// TODO
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct InterruptHandler(Captr);

impl Deref for InterruptHandler {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for InterruptHandler {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::InterruptHandler;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

impl Deref for InterruptPool {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

/// TODO
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct InterruptPool(Captr);

impl Capability for InterruptPool {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::InterruptPool;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

/// A synchronous rendezvous IPC endpoint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Endpoint(Captr);

impl Deref for Endpoint {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for Endpoint {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Endpoint;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

/// A [`Captr`] with additional information about its type, rights,
/// and badge.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct AnnotatedCaptr {
    /// The inner `Captr`.
    pub cap: Captr,
    /// What type is this captr?
    pub ty: CapabilityType,
    /// What rights does this captr have?
    pub rights: CapRights,
    /// What is this captr's badge, if any?
    pub badge: Option<NonZeroU64>,
}

impl Default for AnnotatedCaptr {
    fn default() -> Self {
        Self {
            cap: Captr::null(),
            ty: CapabilityType::Unknown,
            rights: CapRights::empty(),
            badge: None,
        }
    }
}

impl Endpoint {
    /// Create a new `Endpoint`.
    ///
    /// # Errors
    ///
    /// TODO
    pub fn create() -> CapResult<Self> {
        // SAFETY: notification_create is always safe.
        let res = unsafe { syscalls::ecall0(SyscallNumber::EndpointCreate) };

        res.map(|x| Captr::from_raw(x as usize))
            .map(Self::from_captr)
            .map_err(CapError::from)
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn recv_with_regs(
        self,
        sender: Option<&mut Option<NonZeroU64>>,
        cap: Option<&mut AnnotatedCaptr>,
    ) -> CapResult<(MessageHeader, [u64; 4])> {
        let mr0: u64;
        let mr1: u64;
        let mr2: u64;
        let mr3: u64;
        let err: u64;
        let val: u64;
        // SAFETY: endpoint_recv is always safe.
        let hdr = unsafe {
            #[rustfmt::skip]
	    core::arch::asm!(
                "ecall",
                in("a0") u64::from(SyscallNumber::EndpointRecv),
                in("a1") self.into_raw() as u64,
                lateout("a0") err,
		lateout("a1") val,
		in("a2") sender.map(|x| x as *mut _ as u64).unwrap_or_default(),
                in("a3") cap.map(|x| x as *mut _ as u64).unwrap_or_default(),
                out("a4") mr0,
                out("a5") mr1,
                out("a6") mr2,
                out("a7") mr3,
            );

            if err != 0 {
                return Err(CapError::from(err));
            }

            val
        };

        Ok((MessageHeader(hdr), [mr0, mr1, mr2, mr3]))
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    #[allow(clippy::similar_names)]
    pub fn call_with_regs(
        self,
        hdr: MessageHeader,
        cap: Option<&mut AnnotatedCaptr>,
        mrs: [u64; 4],
    ) -> CapResult<(MessageHeader, [u64; 4])> {
        let mut mr0: u64 = mrs[0];
        let mut mr1: u64 = mrs[1];
        let mut mr2: u64 = mrs[2];
        let mut mr3: u64 = mrs[3];
        let err: u64;
        let val: u64;
        // SAFETY: endpoint_recv is always safe.
        let hdr = unsafe {
            #[rustfmt::skip]
            core::arch::asm!(
                "ecall",
                in("a0") u64::from(SyscallNumber::EndpointCall),
		in("a1") self.into_raw() as u64,
		lateout("a0") err,
                lateout("a1") val,
		in("a2") hdr.0,
                in("a3") cap.map(|x| x as *mut _ as u64).unwrap_or_default(),
                inout("a4") mr0,
                inout("a5") mr1,
                inout("a6") mr2,
                inout("a7") mr3
            );

            if err != 0 {
                return Err(CapError::from(err));
            }

            val
        };

        Ok((MessageHeader(hdr), [mr0, mr1, mr2, mr3]))
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn send_with_regs(self, hdr: MessageHeader, cap: Captr, mrs: [u64; 4]) -> CapResult<()> {
        // SAFETY: endpoint_send is always safe.
        let res = unsafe {
            syscalls::ecall7(
                SyscallNumber::EndpointSend,
                self.into_raw() as u64,
                hdr.0,
                ptr::addr_of!(cap) as u64,
                mrs[0],
                mrs[1],
                mrs[2],
                mrs[3],
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn reply_with_regs(self, hdr: MessageHeader, cap: Captr, mrs: [u64; 4]) -> CapResult<()> {
        // SAFETY: endpoint_send is always safe.
        let res = unsafe {
            syscalls::ecall7(
                SyscallNumber::EndpointReply,
                self.into_raw() as u64,
                hdr.0,
                ptr::addr_of!(cap) as u64,
                mrs[0],
                mrs[1],
                mrs[2],
                mrs[3],
            )
        };

        if let Err(e) = res {
            Err(CapError::from(e))
        } else {
            Ok(())
        }
    }
}

/// A job is a group of [`Thread`]s that share resources via
/// capabilities, as well as configurable policies and limits.
///
/// Jobs can also have child jobs, and each job except for the root
/// job has a parent job. These child jobs inherit policies and limits
/// (any configuration must be a subset of the parent jobs' limits),
/// but do not inherit capabilities. This way, proper isolation can be
/// ensured.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Job(Captr);

impl Deref for Job {
    type Target = Captr;
    fn deref(&self) -> &Captr {
        &self.0
    }
}

impl Capability for Job {
    const CAPABILITY_TYPE: CapabilityType = CapabilityType::Job;

    fn from_captr(c: Captr) -> Self {
        Self(c)
    }
}

impl Job {
    /// Create a new job with the given job as its parent.
    ///
    /// Note that, to prevent unbounded recursion, the "height" of the
    /// job tree is limited to 32. This limitation is tracked and
    /// restricted by the kernel.
    ///
    /// # Errors
    ///
    /// - [`CapError::NotPresent`]: `self` or `parent` was null.
    ///
    /// - [`CapError::InvalidOperation`]: The job tree's height would
    ///   be greater than 32.
    pub fn create(parent: Job) -> CapResult<Self> {
        // SAFETY: job_create is always safe.
        let res = unsafe { syscalls::ecall1(SyscallNumber::JobCreate, parent.into_raw() as u64) };

        res.map(|x| x as usize)
            .map(Captr::from_raw)
            .map(Job::from_captr)
            .map_err(CapError::from)
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn link(self, _thread: Thread) -> CapResult<()> {
        todo!()
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn create_thread(self, name: &str) -> CapResult<Thread> {
        // SAFETY: By invariants.
        let res = unsafe {
            syscalls::ecall3(
                SyscallNumber::JobCreateThread,
                self.into_raw() as u64,
                name.as_ptr() as u64,
                name.len() as u64,
            )
        };

        res.map(|x| x as usize)
            .map(Captr::from_raw)
            .map(Thread::from_captr)
            .map_err(CapError::from)
    }
}

/// A header for an IPC message. Consists of a 9-bit length (1 to 512)
/// and a arbitrary, user-defined 55-bit private value.
///
/// Note that message lengths are in units of words, not bytes.
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct MessageHeader(u64);

impl From<u64> for MessageHeader {
    fn from(value: u64) -> Self {
        Self::from_raw(value)
    }
}

impl From<MessageHeader> for u64 {
    fn from(value: MessageHeader) -> Self {
        value.into_raw()
    }
}

impl MessageHeader {
    const LENGTH_MASK: u64 = 0b1_1111_1111;
    const PRIVATE_MASK: u64 = !Self::LENGTH_MASK;

    /// Create a new, empty `MessageHeader` with a minimum length.
    pub const fn new() -> Self {
        Self(0)
    }

    /// Convert a `MessageHeader` from its raw `u64` representation.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Get the length of this `MessageHeader`. Note that this is in
    /// units of words, not bytes.
    pub const fn length(self) -> usize {
        ((self.0 & Self::LENGTH_MASK) as usize) + 1
    }

    /// Get the private value of this `MessageHeader`.
    pub const fn private(self) -> u64 {
        (self.0 & Self::PRIVATE_MASK) >> 9
    }

    /// Set the length of the provided `MessageHeader`, and return the
    /// new header.
    ///
    /// Note that the length is in units of words, not bytes.
    #[must_use]
    pub const fn with_length(self, length: usize) -> Self {
        let without_length = self.0 & !Self::LENGTH_MASK;
        Self(without_length | (length.saturating_sub(1) as u64 & Self::LENGTH_MASK))
    }

    /// Set the private value of the provided `MessageHeader`, and
    /// return the new header.
    #[must_use]
    pub const fn with_private(self, private: u64) -> Self {
        let without_private = self.0 & !Self::PRIVATE_MASK;
        let private = private << 9;
        Self(without_private | (private & Self::PRIVATE_MASK))
    }

    /// Convert a `MessageHeader` into its raw `u64` representation.
    pub const fn into_raw(self) -> u64 {
        self.0
    }
}

// == Unimportant or boilerplate-y impls below ==

impl fmt::Debug for MessageHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageHeader")
            .field("length", &self.length())
            .field("private", &self.private())
            .finish()
    }
}

mod private {
    use super::{
        paging::{BasePage, GigaPage, MegaPage, Page, PageTable, PagingLevel},
        AnyCap, Empty, Endpoint, InterruptHandler, InterruptPool, Job, Notification, Thread,
    };

    pub trait Sealed {}
    impl Sealed for GigaPage {}
    impl Sealed for MegaPage {}
    impl Sealed for BasePage {}
    #[cfg(any(doc, feature = "kernel"))]
    impl Sealed for super::paging::DynLevel {}
    impl<L: PagingLevel> Sealed for Page<L> {}
    impl Sealed for PageTable {}
    impl Sealed for Empty {}
    impl Sealed for Thread {}
    impl Sealed for Notification {}
    impl Sealed for AnyCap {}
    impl Sealed for InterruptPool {}
    impl Sealed for InterruptHandler {}
    impl Sealed for Endpoint {}
    impl Sealed for Job {}
}

impl PartialOrd for Captr {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Captr {
    fn eq(&self, other: &Self) -> bool {
        self.inner.eq(&other.inner)
    }
}

impl Eq for Captr {}

impl Ord for Captr {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl Hash for Captr {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl fmt::Debug for Captr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Captr({})", self.inner.map_or(0, NonZeroUsize::get))
    }
}

impl<C: Capability> fmt::Debug for CaptrRange<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CaptrRange({:?}..{:?})", self.lo, self.hi)
    }
}
