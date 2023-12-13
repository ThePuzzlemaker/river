use core::fmt;
use core::marker::PhantomData;
use core::num::NonZeroU64;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize};
use core::{cmp, mem};

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use atomic::Ordering;
use bytemuck::NoUninit;
use num_enum::{FromPrimitive, IntoPrimitive};
use rille::capability::paging::{
    BasePage, DynLevel, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel,
};
use rille::capability::{
    CapError, CapResult, CapRights, CapabilityType, MessageHeader, UserRegisters,
};

pub use rille::capability::{
    paging::PageTable as PageTableCap, AnyCap, Captbl as CaptblCap, Empty as EmptyCap,
    Endpoint as EndpointCap, InterruptHandler as InterruptHandlerCap,
    InterruptPool as InterruptPoolCap, Notification as NotificationCap, Thread as ThreadCap,
};

pub type PageCap = rille::capability::paging::Page<DynLevel>;

use rille::addr::{DirectMapped, Identity, Kernel, PhysicalMut, VirtualConst};
use rille::symbol;
use rille::units::{self, StorageUnits};

use crate::asm::InterruptDisabler;
use crate::hart_local::LOCAL_HART;
use crate::kalloc::phys::PMAlloc;
use crate::paging::SharedPageTable;
use crate::plic::PLIC;
use crate::proc::Context;
use crate::sched::Scheduler;
use crate::sync::{OnceCell, SpinMutex, SpinMutexGuard, SpinRwLock, SpinRwLockWriteGuard};
use crate::trampoline::Trapframe;
use crate::trap::user_trap_ret;
use crate::{asm, kalloc};

use self::captbl::{Captbl, WeakCaptbl};
use self::slotref::{SlotRef, SlotRefMut};

pub mod captbl;
pub mod slotref;

#[repr(C, align(64))]
#[derive(Debug, Default)]
pub struct CapabilitySlot {
    pub lock: SpinRwLock<CapabilitySlotInner>,
}

const _ASSERT_CAPSLOT_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CapabilitySlot>() == 64,
    "RawCapabilitySlot size was not 64 bytes"
);

#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilitySlotInner {
    pub cap: CapabilityKind,
    pub badge: Option<NonZeroU64>,
    pub rights: CapRights,
}

#[repr(u8)]
#[derive(Clone)]
pub enum CapabilityKind {
    Empty = CapabilityType::Empty as u8,
    Captbl(WeakCaptbl) = CapabilityType::Captbl as u8,
    PgTbl(SharedPageTable) = CapabilityType::PgTbl as u8,
    Page(Page) = CapabilityType::Page as u8,
    Thread(Arc<Thread>) = CapabilityType::Thread as u8,
    Notification(Arc<Notification>) = CapabilityType::Notification as u8,
    InterruptPool(Arc<InterruptPool>) = CapabilityType::InterruptPool as u8,
    InterruptHandler(Arc<InterruptHandler>) = CapabilityType::InterruptHandler as u8,
    Endpoint(Arc<Endpoint>) = CapabilityType::Endpoint as u8,
}

impl fmt::Debug for CapabilityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Empty"),
            Self::Captbl(captbl) => f.debug_tuple("Captbl").field(captbl).finish(),
            Self::PgTbl(pgtbl) => f.debug_tuple("PgTbl").field(pgtbl).finish(),
            Self::Page(page) => f.debug_tuple("Page").field(page).finish(),
            Self::Thread(thread) => write!(f, "Thread(<opaque:{:#p}>", Arc::as_ptr(thread)),
            Self::Notification(notif) => f.debug_tuple("Notification").field(notif).finish(),
            Self::InterruptHandler(handler) => {
                f.debug_tuple("InterruptHandler").field(handler).finish()
            }
            Self::InterruptPool(pool) => f.debug_tuple("InterruptPool").field(pool).finish(),
            Self::Endpoint(endpoint) => f.debug_tuple("Endpoint").field(endpoint).finish(),
        }
    }
}

impl CapabilityKind {
    pub fn cap_type(&self) -> CapabilityType {
        // SAFETY: Because `Self` is marked `repr(u8)`, its layout is a `repr(C)` `union`
        // between `repr(C)` structs, each of which has the `u8` discriminant as its first
        // field, so we can read the discriminant without offsetting the pointer.
        let x = unsafe { *<*const _>::from(self).cast::<u8>() };
        CapabilityType::from(x)
    }

    pub fn captbl(&self) -> Option<&WeakCaptbl> {
        if let Self::Captbl(captbl) = self {
            Some(captbl)
        } else {
            None
        }
    }

    pub fn pgtbl(&self) -> Option<&SharedPageTable> {
        if let Self::PgTbl(pgtbl) = self {
            Some(pgtbl)
        } else {
            None
        }
    }

    pub fn page(&self) -> Option<&Page> {
        if let Self::Page(page) = self {
            Some(page)
        } else {
            None
        }
    }

    pub fn page_mut(&mut self) -> Option<&mut Page> {
        if let Self::Page(page) = self {
            Some(page)
        } else {
            None
        }
    }

    pub fn thread(&self) -> Option<&Arc<Thread>> {
        if let Self::Thread(thread) = self {
            Some(thread)
        } else {
            None
        }
    }

    pub fn notification(&self) -> Option<&Arc<Notification>> {
        if let Self::Notification(notif) = self {
            Some(notif)
        } else {
            None
        }
    }

    pub fn intr_handler(&self) -> Option<&Arc<InterruptHandler>> {
        if let Self::InterruptHandler(handler) = self {
            Some(handler)
        } else {
            None
        }
    }

    pub fn intr_pool(&self) -> Option<&Arc<InterruptPool>> {
        if let Self::InterruptPool(pool) = self {
            Some(pool)
        } else {
            None
        }
    }

    pub fn endpoint(&self) -> Option<&Arc<Endpoint>> {
        if let Self::Endpoint(ep) = self {
            Some(ep)
        } else {
            None
        }
    }
}

impl Default for CapabilityKind {
    fn default() -> Self {
        Self::Empty
    }
}

impl PartialEq for CapabilityKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Captbl(l), Self::Captbl(r)) => l.eq(r),
            (Self::PgTbl(l), Self::PgTbl(r)) => l.eq(r),
            (Self::Page(l), Self::Page(r)) => l.eq(r),
            (Self::Thread(l), Self::Thread(r)) => Arc::ptr_eq(l, r),
            (Self::Notification(l), Self::Notification(r)) => Arc::ptr_eq(l, r),
            _ => mem::discriminant(self) == mem::discriminant(other),
        }
    }
}

impl Eq for CapabilityKind {}

pub trait Capability: rille::capability::Capability
where
    Self::Value: CapabilityValue<Cap = Self>,
{
    type Value;
    fn is_valid_type(cap_type: CapabilityType) -> bool;
}

#[derive(Debug)]
#[repr(align(64))]
pub struct CaptblHeader {
    n_slots_log2: u8,
    slot_rc: AtomicUsize,
}

const _ASSERT_CAPTBLHEADER_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CaptblHeader>() == 64,
    "CaptblHeader size was not 64 bytes"
);

impl Captbl {
    /// TODO
    ///
    /// # Errors
    ///
    /// If the capability at this slot was out of bounds,
    /// [`CapError::NotPresent`] will be returned. If the capability
    /// was of the wrong type, [`CapError::InvalidType`] is returned.
    pub fn get<C: Capability>(&self, index: usize) -> CapResult<SlotRef<'_, C>> {
        if index == 0 {
            return Err(CapError::NotPresent);
        }

        // SAFETY: By invariants.
        let slots = unsafe { &*self.inner.0 };

        let slot = slots.get(index).ok_or(CapError::NotPresent)?;
        // SAFETY: By invariants.
        let slot = unsafe { &*slot.slot };
        let guard = slot.lock.read();

        if !C::is_valid_type(guard.cap.cap_type()) {
            return Err(CapError::InvalidType);
        }

        Ok(SlotRef {
            slot: guard,
            _phantom: PhantomData,
        })
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// If the capability at this slot was out of bounds,
    /// [`CapError::NotPresent`] will be returned. If the capability
    /// was of the wrong type, [`CapError::InvalidType`] is returned.
    #[track_caller]
    pub fn get_mut<C: Capability>(&self, index: usize) -> CapResult<SlotRefMut<'_, C>> {
        if index == 0 {
            return Err(CapError::NotPresent);
        }

        // SAFETY: By invariants.
        let slots = unsafe { &*self.inner.0 };

        let slot = slots.get(index).ok_or(CapError::NotPresent)?;
        // SAFETY: By invariants.
        let slot = unsafe { &*slot.slot };
        let guard = slot.lock.write();

        if !C::is_valid_type(guard.cap.cap_type()) {
            return Err(CapError::InvalidType);
        }

        Ok(SlotRefMut {
            slot: guard,
            _phantom: PhantomData,
        })
    }
}

pub trait CapToOwned {
    type Target;

    fn to_owned_cap(&self) -> Self::Target;
}

impl<'a> CapToOwned for SlotRef<'a, EmptyCap> {
    type Target = ();

    fn to_owned_cap(&self) {}
}

impl<'a> CapToOwned for SlotRefMut<'a, EmptyCap> {
    type Target = ();

    fn to_owned_cap(&self) {}
}

#[derive(Clone, Debug)]
pub enum AnyCapVal {
    Empty,
    Captbl(WeakCaptbl),
    Page(Page),
    PgTbl(SharedPageTable),
    Thread(Arc<Thread>),
    Notification(Arc<Notification>),
    InterruptHandler(Arc<InterruptHandler>),
    InterruptPool(Arc<InterruptPool>),
    Endpoint(Arc<Endpoint>),
    // TODO
}

impl<'a> CapToOwned for SlotRef<'a, AnyCap> {
    type Target = AnyCapVal;

    fn to_owned_cap(&self) -> AnyCapVal {
        match &self.cap {
            CapabilityKind::Empty => AnyCapVal::Empty,
            CapabilityKind::Captbl(tbl) => AnyCapVal::Captbl(tbl.clone()),
            CapabilityKind::Page(page) => AnyCapVal::Page(page.clone()),
            CapabilityKind::PgTbl(pgtbl) => AnyCapVal::PgTbl(pgtbl.clone()),
            CapabilityKind::Thread(thread) => AnyCapVal::Thread(thread.clone()),
            CapabilityKind::Notification(notification) => {
                AnyCapVal::Notification(notification.clone())
            }
            CapabilityKind::InterruptPool(pool) => AnyCapVal::InterruptPool(pool.clone()),
            CapabilityKind::InterruptHandler(handler) => {
                AnyCapVal::InterruptHandler(handler.clone())
            }
            CapabilityKind::Endpoint(endpoint) => AnyCapVal::Endpoint(endpoint.clone()),
        }
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, AnyCap> {
    type Target = AnyCapVal;

    fn to_owned_cap(&self) -> AnyCapVal {
        match &self.cap {
            CapabilityKind::Empty => AnyCapVal::Empty,
            CapabilityKind::Captbl(tbl) => AnyCapVal::Captbl(tbl.clone()),
            CapabilityKind::Page(page) => AnyCapVal::Page(page.clone()),
            CapabilityKind::PgTbl(pgtbl) => AnyCapVal::PgTbl(pgtbl.clone()),
            CapabilityKind::Thread(thread) => AnyCapVal::Thread(thread.clone()),
            CapabilityKind::Notification(notification) => {
                AnyCapVal::Notification(notification.clone())
            }

            CapabilityKind::InterruptPool(pool) => AnyCapVal::InterruptPool(pool.clone()),
            CapabilityKind::InterruptHandler(handler) => {
                AnyCapVal::InterruptHandler(handler.clone())
            }
            CapabilityKind::Endpoint(endpoint) => AnyCapVal::Endpoint(endpoint.clone()),
        }
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn set_badge(&mut self, badge: Option<NonZeroU64>) {
        self.badge = badge;
    }

    pub fn update_rights(&mut self, rights: CapRights) {
        self.rights &= rights;
    }
}

pub trait CapabilityValue {
    type Cap: Capability;
    fn into_kind(self) -> CapabilityKind;
    /// # Safety
    ///
    /// The slot must be of the correct type.
    unsafe fn copy_hook(_slot: &SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {}
    /// # Safety
    ///
    /// This function must only be called once per slot, before the
    /// slot has been cleared. Additionally, the slot must be of the
    /// correct type.
    unsafe fn delete_hook(_slot: &mut SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {}
}
impl CapabilityValue for Empty {
    type Cap = EmptyCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Empty
    }
}
impl CapabilityValue for Page {
    type Cap = PageCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Page(self)
    }
}
impl CapabilityValue for SharedPageTable {
    type Cap = PageTableCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::PgTbl(self)
    }
}
impl CapabilityValue for Arc<Thread> {
    type Cap = ThreadCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Thread(self)
    }

    unsafe fn copy_hook(slot: &SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: By invariants.
        let thread = unsafe { slot.cap.thread().unwrap_unchecked() };
        assert!(
            thread.slot_refcount.fetch_add(1, Ordering::Relaxed) <= (u32::MAX - 1) as u64,
            "too many references to thread"
        );
    }

    unsafe fn delete_hook(slot: &mut SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: By invariants.
        let thread = unsafe { slot.cap.thread().unwrap_unchecked() };

        // If we're an orphan, extract our captbl and drop it
        // This breaks any cycles that prevent us from being fully
        // dropped.
        if thread.slot_refcount.fetch_sub(1, Ordering::Release) == 1 {
            atomic::fence(Ordering::Acquire);
            let mut private = thread.private.lock();
            drop(private.captbl.take());
            drop(private);
        }
    }
}
impl CapabilityValue for Arc<Notification> {
    type Cap = NotificationCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Notification(self)
    }
}
impl CapabilityValue for WeakCaptbl {
    type Cap = CaptblCap;
    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Captbl(self)
    }

    unsafe fn copy_hook(slot: &SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: By invariants.
        let captbl = unsafe { slot.cap.captbl().unwrap_unchecked() };
        if let Some(captbl) = captbl.upgrade() {
            let x = captbl.hdr().slot_rc.fetch_add(1, Ordering::Relaxed);

            if x == 0 {
                // Carefully leak this captbl.
                let _ = Arc::into_raw(captbl.inner.clone());
            }
            assert!(
                x <= (u32::MAX - 1) as usize,
                "too many references to captbl"
            );
        }
    }

    unsafe fn delete_hook(slot: &mut SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: By invariants.
        let captbl = unsafe { slot.cap.captbl().unwrap_unchecked() };
        if let Some(captbl) = captbl.upgrade() {
            // If we're an orphan, un-leak ourself.
            if captbl.hdr().slot_rc.fetch_sub(1, Ordering::Release) == 1 {
                atomic::fence(Ordering::Acquire);
                let x = Arc::into_raw(captbl.inner.clone());
                // SAFETY: This pointer was obtained from into_raw,
                // and if our slot_rc was > 0, we have at least one
                // leaked captbl.
                unsafe {
                    Arc::decrement_strong_count(x);
                }
                // SAFETY: This pointer was obtained from into_raw.
                drop(unsafe { Arc::from_raw(x) });
            }
        }
    }
}
impl CapabilityValue for AnyCapVal {
    type Cap = AnyCap;
    fn into_kind(self) -> CapabilityKind {
        match self {
            AnyCapVal::Empty => Empty.into_kind(),
            AnyCapVal::Captbl(captbl) => captbl.into_kind(),
            AnyCapVal::Page(pg) => pg.into_kind(),
            AnyCapVal::PgTbl(pgtbl) => pgtbl.into_kind(),
            AnyCapVal::Thread(thread) => thread.into_kind(),
            AnyCapVal::Notification(notif) => notif.into_kind(),
            AnyCapVal::InterruptHandler(handler) => handler.into_kind(),
            AnyCapVal::InterruptPool(pool) => pool.into_kind(),
            AnyCapVal::Endpoint(endpoint) => endpoint.into_kind(),
        }
    }

    unsafe fn copy_hook(slot: &SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: All by invariants.
        unsafe {
            match slot.cap {
                CapabilityKind::Empty => Empty::copy_hook(slot),
                CapabilityKind::Captbl(_) => WeakCaptbl::copy_hook(slot),
                CapabilityKind::PgTbl(_) => SharedPageTable::copy_hook(slot),
                CapabilityKind::Page(_) => Page::copy_hook(slot),
                CapabilityKind::Thread(_) => Arc::<Thread>::copy_hook(slot),
                CapabilityKind::Notification(_) => Arc::<Notification>::copy_hook(slot),
                CapabilityKind::InterruptHandler(_) => Arc::<InterruptHandler>::copy_hook(slot),
                CapabilityKind::InterruptPool(_) => Arc::<InterruptPool>::copy_hook(slot),
                CapabilityKind::Endpoint(_) => Arc::<Endpoint>::copy_hook(slot),
            }
        }
    }

    unsafe fn delete_hook(slot: &mut SpinRwLockWriteGuard<'_, CapabilitySlotInner>) {
        // SAFETY: All by invariants.
        unsafe {
            match slot.cap {
                CapabilityKind::Empty => Empty::delete_hook(slot),
                CapabilityKind::Captbl(_) => WeakCaptbl::delete_hook(slot),
                CapabilityKind::PgTbl(_) => SharedPageTable::delete_hook(slot),
                CapabilityKind::Page(_) => Page::delete_hook(slot),
                CapabilityKind::Thread(_) => Arc::<Thread>::delete_hook(slot),
                CapabilityKind::Notification(_) => Arc::<Notification>::delete_hook(slot),
                CapabilityKind::InterruptHandler(_) => Arc::<InterruptHandler>::delete_hook(slot),
                CapabilityKind::InterruptPool(_) => Arc::<InterruptPool>::delete_hook(slot),
                CapabilityKind::Endpoint(_) => Arc::<Endpoint>::delete_hook(slot),
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Empty;
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Allocator;

impl<'a> SlotRefMut<'a, EmptyCap> {
    pub fn replace<T: CapabilityValue>(self, val: T) -> SlotRefMut<'a, T::Cap> {
        let mut slot = SlotRefMut {
            slot: self.slot,
            _phantom: PhantomData,
        };
        slot.slot.cap = T::into_kind(val);
        slot.slot.badge = None;
        slot.slot.rights = CapRights::all();
        // SAFETY: We have just initialized this slot with the new
        // type.
        unsafe {
            T::copy_hook(&slot.slot);
        }
        slot
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn delete(mut self) -> SlotRefMut<'a, EmptyCap> {
        // SAFETY: We will only call this once, and we have not yet
        // invalidated the slot.
        unsafe { C::Value::delete_hook(&mut self.slot) };

        let mut slot = SlotRefMut {
            slot: self.slot,
            _phantom: PhantomData,
        };

        slot.cap = CapabilityKind::Empty;
        slot.badge = None;
        slot.rights = CapRights::all();

        slot
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub phys: PhysicalMut<u8, DirectMapped>,
    pub size_log2: u8,
    pub is_device: bool,
}

impl Page {
    /// Create a page capability from a physical memory allocation.
    ///
    /// # Safety
    ///
    /// - `phys` must have been allocated by [`PMAlloc`].
    /// - `phys` must be valid for `1 << size_log2` bytes.
    /// - `phys` must be aligned to `1 << size_log2` bytes.
    // TODO: dealloc?
    pub unsafe fn new(phys: PhysicalMut<u8, DirectMapped>, size_log2: u8) -> Self {
        Self {
            phys,
            size_log2,
            is_device: false,
        }
    }

    /// Create a page capability from MMIO.
    ///
    /// # Safety
    ///
    /// See [`Self::new`].
    pub unsafe fn new_device(phys: PhysicalMut<u8, DirectMapped>, size_log2: u8) -> Self {
        Self {
            phys,
            size_log2,
            is_device: true,
        }
    }
}

impl<'a> SlotRef<'a, PageTableCap> {
    pub fn table(&self) -> &SharedPageTable {
        // SAFETY: Type check.
        unsafe { self.slot.cap.pgtbl().unwrap_unchecked() }
    }
}

impl<'a> SlotRefMut<'a, PageTableCap> {
    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn map_page(
        &mut self,
        page: &mut SlotRefMut<'_, PageCap>,
        addr: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) -> CapResult<()> {
        use crate::paging::PageTableFlags as KernelFlags;

        // SAFETY: Type check.
        let page_cap = unsafe { page.cap.page().unwrap_unchecked() };
        // SAFETY: Type check.
        let pgtbl = unsafe { self.cap.pgtbl().unwrap_unchecked() };

        if addr.into_usize() >= 0x40_0000_0000 {
            return Err(CapError::InvalidOperation);
        }

        if addr.into_usize() % (1 << (page_cap.size_log2 as usize)) != 0 {
            return Err(CapError::InvalidSize);
        }

        let flags = flags & page.rights.into_pgtbl_mask();

        let flags =
            KernelFlags::from_bits_truncate(flags.bits()) | KernelFlags::VAD | KernelFlags::USER;

        {
            let page_size = match page_cap.size_log2 as usize {
                BasePage::PAGE_SIZE_LOG2 => PageSize::Base,
                MegaPage::PAGE_SIZE_LOG2 => PageSize::Mega,
                GigaPage::PAGE_SIZE_LOG2 => PageSize::Giga,
                _ => unreachable!(),
            };
            pgtbl.map(
                page_cap.phys.into_identity().into_const(),
                addr,
                flags,
                page_size,
            );
        }
        // TODO: sfence
        //self.add_child(page);

        Ok(())
    }
}

/*






*/

// static NEXT_ASID: AtomicU16 = AtomicU16::new(1);

// fn next_asid() -> u16 {
//     let v = NEXT_ASID.fetch_add(1, Ordering::Relaxed);
//     assert!(
//         core::intrinsics::likely(v != u16::MAX),
//         "oops! ran out of ASIDs"
//     );
//     v
// }

static NEXT_TID: AtomicU16 = AtomicU16::new(1);

fn next_tid() -> u16 {
    let v = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    assert!(
        core::intrinsics::likely(v != u16::MAX),
        "oops! ran out of TIDs"
    );
    v
}

impl<'a> SlotRef<'a, ThreadCap> {
    pub fn suspend(&self) {
        // SAFETY: Type check.
        let thread = unsafe { self.cap.thread().unwrap_unchecked() };

        thread.private.lock().state = ThreadState::Suspended;
    }

    /// Configure the captbl and page table of a thread.
    ///
    /// # Errors
    ///
    /// See [`Captr::<Thread>::configure`][1].
    ///
    /// [1]: rille::capability::Captr::<rille::capability::Thread>::configure
    pub fn configure(
        &self,
        captbl: Captbl,
        pgtbl: SharedPageTable,
        ipc_buffer: Option<PhysicalMut<u8, DirectMapped>>,
    ) -> CapResult<()> {
        use crate::paging::PageTableFlags as KernelFlags;
        // SAFETY: Type check.
        let thread = unsafe { self.cap.thread().unwrap_unchecked() };

        let mut private = thread.private.lock();
        if private.state != ThreadState::Suspended && private.state != ThreadState::Uninit {
            return Err(CapError::InvalidOperation);
        }

        private.captbl = Some(captbl);
        let old_pgtbl = private.root_pgtbl.replace(pgtbl);
        if let Some(_pgtbl) = old_pgtbl {
            // TODO: pgtbl.unmap(VirtualConst::from(private.trapframe_addr))
        }
        let trampoline_virt =
            VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());

        // SAFETY: Type check.
        unsafe { private.root_pgtbl.as_mut().unwrap_unchecked() }.map(
            trampoline_virt.into_phys().into_identity(),
            VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
            KernelFlags::VAD | KernelFlags::RX,
            PageSize::Base,
        );

        private.state = ThreadState::Suspended;
        private.ipc_buffer = ipc_buffer;

        drop(private);
        thread.setup_page_table();

        Ok(())
    }

    /// Resume a thread.
    ///
    /// # Errors
    ///
    /// See [`Captr::<Thread>::resume`][1].
    ///
    /// [1]: rille::capability::Captr::<rille::capability::Thread>::resume
    pub fn resume(&self) -> CapResult<()> {
        // SAFETY: Type check.
        let thread = unsafe { self.cap.thread().unwrap_unchecked() };

        let cont = {
            let private = thread.private.lock();
            private.root_pgtbl.is_some()
        };

        if !cont {
            return Err(CapError::InvalidOperation);
        }

        thread.private.lock().state = ThreadState::Runnable;
        Scheduler::enqueue_dl(thread, None, &mut thread.private.lock());

        Ok(())
    }

    /// Write the trapframe a thread.
    ///
    /// # Errors
    ///
    /// See [`Captr::<Thread>::write_regsters`][1].
    ///
    /// [1]: rille::capability::Captr::<rille::capability::Thread>::write_registers
    pub fn write_registers(
        &self,
        registers: VirtualConst<UserRegisters, Identity>,
    ) -> CapResult<()> {
        use crate::paging::PageTableFlags as KernelFlags;

        // SAFETY: Type check.
        let thread = unsafe { self.cap.thread().unwrap_unchecked() };

        let private = thread.private.lock();

        if private.state != ThreadState::Suspended {
            return Err(CapError::InvalidOperation);
        }

        let Some((addr, flags)) = private
            .root_pgtbl
            .as_ref()
            .ok_or(CapError::InvalidOperation)?
            .walk(registers)
        else {
            return Err(CapError::InvalidOperation);
        };
        if !flags.contains(KernelFlags::USER) {
            return Err(CapError::InvalidOperation);
        }
        // SAFETY: This pointer is valid and non-null.
        let registers = unsafe { &*addr.into_virt().cast::<UserRegisters>().into_ptr() };

        let mut trapframe = thread.trapframe.lock();
        let trapframe = &mut **trapframe;
        let old_trapframe = &*trapframe;
        *trapframe = Trapframe {
            ra: registers.ra,
            sp: registers.sp,
            gp: registers.gp,
            tp: registers.tp,
            t0: registers.t0,
            t1: registers.t1,
            t2: registers.t2,
            s0: registers.s0,
            s1: registers.s1,
            a0: registers.a0,
            a1: registers.a1,
            a2: registers.a2,
            a3: registers.a3,
            a4: registers.a4,
            a5: registers.a5,
            a6: registers.a6,
            a7: registers.a7,
            s2: registers.s2,
            s3: registers.s3,
            s4: registers.s4,
            s5: registers.s5,
            s6: registers.s6,
            s7: registers.s7,
            s8: registers.s8,
            s9: registers.s9,
            s10: registers.s10,
            s11: registers.s11,
            t3: registers.t3,
            t4: registers.t4,
            t5: registers.t5,
            t6: registers.t6,
            user_epc: registers.pc,
            ..*old_trapframe
        };

        Ok(())
    }
}

pub struct Thread {
    pub trapframe: SpinMutex<OwnedTrapframe>,
    pub stack: ThreadKernelStack,
    pub private: SpinMutex<ThreadProtected>,
    pub context: SpinMutex<Context>,
    pub tid: usize,
    pub waiting_on: AtomicU64,
    pub slot_refcount: AtomicU64,
    pub blocking_wqs: SpinMutex<Vec<Arc<SpinMutex<WaitQueue>>>>,
    pub blocked_on_wq: SpinMutex<Option<Weak<SpinMutex<WaitQueue>>>>,
    pub affinity: SpinMutex<HartMask>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct HartMask(pub u64);

impl Thread {
    pub fn queue_pressure_changed(
        self: Arc<Thread>,
        mut old_prio: i8,
        mut new_prio: i8,
        accum_cpu_mask: &mut HartMask,
    ) -> bool {
        let inherited_prio = self.private.lock().prio.inherited_prio();
        let mut local_resched = false;

        let mut this = self;
        loop {
            if new_prio < old_prio {
                // If the priority just dropped, but the old priority
                // was strictly lower than the current inherited
                // priority of the thread, then there is nothing to
                // do. We can just stop. The maximum inherited
                // priority must have come from a different wait
                // queue.
                if old_prio < inherited_prio {
                    return local_resched;
                }

                // Prio dropped. We must recalculate the max from our
                // blocking wait queues.
                for wq in this.blocking_wqs.lock().iter() {
                    let prio = wq.lock().highest_prio();

                    new_prio = cmp::max(new_prio, prio);
                }

                // If our new priority is still the same as our
                // current inherited priority, then we are done.
                if new_prio == inherited_prio {
                    return local_resched;
                }
            } else {
                // If the priority just went up, but it's not greater
                // than our current inherited priority, then there is
                // nothing to do.
                if new_prio <= inherited_prio {
                    return local_resched;
                }
            }

            // let old_effec_prio = prios.effective();
            // let old_inherited_prio = prios.inherited_prio;
            let old_queue_prio =
                if let Some(wq) = this.blocked_on_wq.lock().as_ref().and_then(Weak::upgrade) {
                    wq.lock().highest_prio()
                } else {
                    -1
                };

            this.inherit_priority(new_prio, &mut local_resched, accum_cpu_mask);

            let new_queue_prio =
                if let Some(wq) = this.blocked_on_wq.lock().as_ref().and_then(Weak::upgrade) {
                    wq.lock().highest_prio()
                } else {
                    -1
                };

            if old_queue_prio == new_queue_prio {
                return local_resched;
            }

            let lock = this.blocked_on_wq.lock();
            if let Some(wq) = lock.as_ref().and_then(Weak::upgrade) {
                drop(lock);
                if let Some(owner) = wq.lock().owner.as_ref().and_then(Weak::upgrade) {
                    this = owner;
                    old_prio = old_queue_prio;
                    new_prio = new_queue_prio;
                    continue;
                }
            } else {
                drop(lock);
            }
        }
    }

    fn inherit_priority(
        self: &Arc<Thread>,
        mut prio: i8,
        local_resched: &mut bool,
        accum_cpu_mask: &mut HartMask,
    ) {
        prio = cmp::min(32, prio);

        let prios = &mut self.private.lock().prio;
        let old_eff_prio = prios.effective();
        prios.inherited_prio = prio;
        let eff_prio = prios.effective();
        if old_eff_prio == eff_prio {
            // same eff. prio, nothing to do
            return;
        }

        crate::sched::Scheduler::prio_changed(
            self,
            old_eff_prio,
            eff_prio,
            local_resched,
            accum_cpu_mask,
            false,
        );
    }

    #[track_caller]
    #[allow(clippy::missing_panics_doc)]
    pub fn wait_queue_priority_changed(
        self: &Arc<Thread>,
        old_prio: i8,
        new_prio: i8,
        propagate: bool,
    ) {
        let wq = {
            self.blocked_on_wq
                .lock()
                .as_ref()
                .and_then(Weak::upgrade)
                .unwrap()
        };

        let mut lock = wq.lock();
        lock.remove(old_prio, self);
        lock.insert(new_prio, self);

        if propagate {
            WaitQueue::waiters_changed(lock, old_prio);
        }
    }
}

#[derive(Debug)]
pub struct WaitQueue {
    inner: BTreeMap<i8, VecDeque<Arc<Thread>>>,
    owner: Option<Weak<Thread>>,
}

impl WaitQueue {
    pub fn new(owner: Option<Weak<Thread>>) -> WaitQueue {
        Self {
            inner: BTreeMap::new(),
            owner,
        }
    }

    pub fn find_highest_thread(&mut self) -> Option<Arc<Thread>> {
        let mut x = None;
        for (_, queue) in self.inner.iter_mut().rev() {
            if let Some(thread) = queue.pop_front() {
                x = Some(thread);
            }
        }
        x
    }

    fn highest_prio(&self) -> i8 {
        let Some(queue) = self.inner.last_key_value() else {
            return -1;
        };
        *queue.0
    }

    fn remove(&mut self, prio: i8, thread: &Arc<Thread>) {
        self.inner
            .get_mut(&prio)
            .unwrap()
            .retain(|x| !Arc::ptr_eq(x, thread));
    }

    pub fn insert(&mut self, prio: i8, thread: &Arc<Thread>) {
        self.inner
            .entry(prio)
            .or_default()
            .push_back(thread.clone());
    }

    fn waiters_changed(this: SpinMutexGuard<'_, Self>, old_prio: i8) -> bool {
        if let Some(owner) = this.owner.as_ref().and_then(Weak::upgrade) {
            if this.highest_prio() != old_prio {
                let new_prio = this.highest_prio();
                let mut mask = HartMask(0);
                drop(this);
                let local_resched = owner.queue_pressure_changed(old_prio, new_prio, &mut mask);
                let cur_hart = asm::hartid();
                let mut sbi_mask = sbi::HartMask::new(0);
                for hart in mask.harts() {
                    if hart != cur_hart {
                        crate::info!("IPI to {hart}");
                        sbi_mask = sbi_mask.with(hart as usize);
                    }
                }
                if sbi_mask != sbi::HartMask::new(0) {
                    log::trace!("{:#?}\n\n", sbi_mask);
                    //   sbi::ipi::send_ipi(sbi_mask).unwrap();
                }
                return local_resched;
            }
        }
        false
    }
}

pub struct OwnedTrapframe {
    inner: PhysicalMut<Trapframe, DirectMapped>,
}

impl OwnedTrapframe {
    /// Allocate a new trapframe.
    ///
    /// # Panics
    ///
    /// This function will panic if the allocation failed.
    pub fn new() -> Self {
        let mut pma = PMAlloc::get();
        Self {
            inner: pma.allocate(0).unwrap().cast(),
        }
    }

    pub fn as_phys(&self) -> PhysicalMut<Trapframe, DirectMapped> {
        self.inner
    }
}

impl Deref for OwnedTrapframe {
    type Target = Trapframe;
    fn deref(&self) -> &Trapframe {
        // SAFETY: By invariants.
        unsafe { &*self.inner.into_virt().into_ptr_mut() }
    }
}

impl DerefMut for OwnedTrapframe {
    fn deref_mut(&mut self) -> &mut Trapframe {
        // SAFETY: By invariants.
        unsafe { &mut *self.inner.into_virt().into_ptr_mut() }
    }
}

impl Default for OwnedTrapframe {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OwnedTrapframe {
    fn drop(&mut self) {
        let mut pma = PMAlloc::get();
        // SAFETY: By invariants.
        unsafe { pma.deallocate(self.inner.cast(), 0) }
    }
}

// SAFETY: A trapframe is just some numbers. Any mutable access
// requires a mutable ref.
unsafe impl Sync for OwnedTrapframe {}
// SAFETY: See above.
unsafe impl Send for OwnedTrapframe {}

impl Thread {
    #[allow(clippy::missing_panics_doc)]
    pub fn new(
        name: String,
        captbl: Option<Captbl>,
        pgtbl: Option<SharedPageTable>,
    ) -> Arc<Thread> {
        let tid = next_tid();

        let this = Arc::new(Thread {
            trapframe: SpinMutex::new(OwnedTrapframe::new()),
            stack: ThreadKernelStack::new(),
            private: SpinMutex::new(ThreadProtected {
                captbl,
                root_pgtbl: pgtbl,
                asid: 1, /*TODO*/
                name,
                trapframe_addr: 0,
                prio: ThreadPriorities {
                    base_prio: 0,
                    inherited_prio: 0,
                    prio_boost: 0,
                },
                state: ThreadState::Uninit,
                hartid: u64::MAX,
                ipc_buffer: None,
                recv_fastpath: false,
                send_fastpath: None,
            }),
            context: SpinMutex::new(Context::default()),
            tid: tid as usize,
            waiting_on: AtomicU64::new(0),
            slot_refcount: AtomicU64::new(0),
            blocking_wqs: SpinMutex::new(Vec::new()),
            blocked_on_wq: SpinMutex::new(None),
            affinity: SpinMutex::new(HartMask(0xFFFF_FFFF_FFFF_FFFF)),
        });

        {
            this.context.lock().ra = user_trap_ret as usize as u64;
            this.context.lock().sp =
                this.stack.inner.into_virt().into_usize() as u64 + THREAD_STACK_SIZE as u64;
        }

        this
    }

    /// Map the trapframe into the page table.
    ///
    /// # Panics
    ///
    /// This function will panic if the trapframe was not mapped in
    /// kernel memory, or if there was no page table configured in the
    /// process.
    pub fn setup_page_table(&self) {
        use crate::paging::PageTableFlags as KernelFlags;

        let mut private = self.private.lock();
        let trapframe_phys = self.trapframe.lock().as_phys();

        let trapframe_mapped_addr = private
            .root_pgtbl
            .as_ref()
            .unwrap()
            .find_free_trapframe_addr()
            .expect("Thread::setup_page_table: too many processes with the same page table");

        // We only need to worry about the first page, as that's
        // where the trapframe will be. Nothing else will be
        // accessed from the user page tables.
        private.root_pgtbl.as_mut().unwrap().map(
            trapframe_phys.into_const().cast().into_identity(),
            trapframe_mapped_addr,
            KernelFlags::VAD | KernelFlags::RW,
            PageSize::Base,
        );

        private.trapframe_addr = trapframe_mapped_addr.into_usize();
    }

    /// Yield the provided process's time to the scheduler, terminally.
    ///
    /// # Safety
    ///
    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    pub unsafe fn yield_to_scheduler_final(self: Arc<Thread>) {
        let intr = InterruptDisabler::new();

        let mut ctx_lock = self.context.lock();
        let ctx = SpinMutex::new(mem::replace(
            &mut *ctx_lock,
            Context {
                ra: user_trap_ret as usize as u64,
                sp: self.stack.inner.into_virt().into_usize() as u64 + THREAD_STACK_SIZE as u64,
                ..Context::default()
            },
        ));
        drop(ctx_lock);
        drop(self);
        drop(intr);
        {
            // SAFETY: The scheduler context is always valid, as
            // guaranteed by the scheduler.
            unsafe { Context::switch(&LOCAL_HART.context, &ctx) }
        }
    }

    /// Yield the running process's time to the scheduler.
    ///
    /// # Safety
    ///
    /// A valid thread must be running on the current hart.
    ///
    /// # Panics
    ///
    /// See above.
    ///
    /// # Guarantees and Deadlocks
    ///
    /// Locks **must not** be held when calling this function, else
    /// there is a large chance for deadlock.
    ///
    /// The scheduler will constrain the user thread's stream of
    /// execution to the current hart. To allow a thread to be stolen
    /// by another hart, the waitlist must be used.
    #[track_caller]
    pub unsafe fn yield_to_scheduler() {
        let intr = InterruptDisabler::new();

        let ctx_ptr = {
            let proc = LOCAL_HART.thread.borrow();
            let proc = proc.as_ref().unwrap();
            core::ptr::addr_of!(proc.context)
        };

        {
            // SAFETY: As the scheduler is currently running this
            // process, it retains an `Arc` to it (We cannot
            // safely dequeue a process without it being suspended
            // first). Thus, this pointer remains valid.
            let context = unsafe { &*ctx_ptr };
            drop(intr);
            // SAFETY: The scheduler context is always valid, as
            // guaranteed by the scheduler.
            unsafe { Context::switch(&LOCAL_HART.context, context) }
        }
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("private", &self.private)
            .field("context", &self.context)
            .field("blocked_on_wq", &self.blocked_on_wq)
            .field("blocking_wqs", &self.blocking_wqs)
            .field("tid", &self.tid)
            .finish_non_exhaustive()
    }
}

pub struct ThreadProtected {
    pub captbl: Option<Captbl>,
    pub root_pgtbl: Option<SharedPageTable>,
    pub asid: u16,
    pub name: String,
    pub trapframe_addr: usize,
    pub prio: ThreadPriorities,
    pub state: ThreadState,
    pub hartid: u64,
    // TODO: handle `Page`s being dropped--make this an Arc
    pub ipc_buffer: Option<PhysicalMut<u8, DirectMapped>>,
    pub send_fastpath: Option<MessageHeader>,
    pub recv_fastpath: bool,
}

#[derive(Debug)]
pub struct ThreadPriorities {
    pub base_prio: i8,
    pub inherited_prio: i8,
    pub prio_boost: i8,
}

impl Thread {
    pub fn prio_boost(self: &Arc<Thread>) {
        self.prio_boost_dl(&mut self.private.lock());
    }

    pub fn prio_boost_dl(self: &Arc<Thread>, private: &mut ThreadProtected) {
        let prio = &mut private.prio;
        let old_eff_prio = prio.effective();
        prio.boost();
        let new_eff_prio = prio.effective();
        if self
            .blocked_on_wq
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            self.wait_queue_priority_changed(old_eff_prio, new_eff_prio, true);
        }
    }

    pub fn prio_diminish_dl(self: &Arc<Thread>, private: &mut ThreadProtected) {
        let prio = &mut private.prio;
        let old_eff_prio = prio.effective();
        prio.diminish();
        let new_eff_prio = prio.effective();
        if self
            .blocked_on_wq
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            self.wait_queue_priority_changed(old_eff_prio, new_eff_prio, true);
        }
    }

    pub fn prio_saturating_diminish_dl(self: &Arc<Thread>, private: &mut ThreadProtected) {
        let prio = &mut private.prio;
        let old_eff_prio = prio.effective();
        prio.saturating_diminish();
        let new_eff_prio = prio.effective();

        if self
            .blocked_on_wq
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            self.wait_queue_priority_changed(old_eff_prio, new_eff_prio, true);
        }
    }
}

impl ThreadPriorities {
    pub fn boost(&mut self) {
        let prio_boost = self.prio_boost();
        if prio_boost < 4 && (self.base_prio() + prio_boost) < 32 {
            self.prio_boost += 1;
        }
    }

    pub fn saturating_diminish(&mut self) {
        let prio_boost = self.prio_boost();
        if prio_boost > 0 && (self.base_prio() + prio_boost) > 0 {
            self.prio_boost -= 1;
        }
    }

    #[inline]
    pub fn prio_boost(&self) -> i8 {
        self.prio_boost
    }

    #[inline]
    pub fn base_prio(&self) -> i8 {
        self.base_prio
    }

    #[inline]
    pub fn inherited_prio(&self) -> i8 {
        self.inherited_prio
    }

    pub fn diminish(&mut self) {
        let prio_boost = self.prio_boost();
        if prio_boost > -4 && (self.base_prio() + prio_boost) > 0 {
            self.prio_boost -= 1;
        }
    }

    pub fn effective(&self) -> i8 {
        cmp::max(self.inherited_prio(), self.base_prio() + self.prio_boost())
    }
}

impl fmt::Debug for ThreadProtected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadProtected")
            .field("root_pgtbl", &self.root_pgtbl)
            .field("asid", &self.asid)
            .field("name", &self.name)
            .field("trapframe_addr", &self.trapframe_addr)
            .field("prio", &self.prio)
            .field("hartid", &self.hartid)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive, NoUninit)]
#[repr(u8)]
pub enum ThreadState {
    #[num_enum(default)]
    Uninit = 0,
    Suspended = 1,
    Blocking = 2,
    Runnable = 3,
    Running = 4,
}

#[repr(C)]
pub struct ThreadKernelStack {
    inner: PhysicalMut<[u8; THREAD_STACK_SIZE], DirectMapped>,
}

impl ThreadKernelStack {
    /// Allocate a new kernel stack for a thread.
    ///
    /// # Panics
    ///
    /// This function will panic if the allocation failed.
    pub fn new() -> Self {
        let mut pma = PMAlloc::get();
        Self {
            inner: pma
                .allocate(kalloc::phys::what_order(THREAD_STACK_SIZE))
                .unwrap()
                .cast(),
        }
    }

    pub fn as_phys(&self) -> PhysicalMut<[u8; THREAD_STACK_SIZE], DirectMapped> {
        self.inner
    }
}

impl Default for ThreadKernelStack {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: The pointer is valid when send across threads, and is only
// accessible as mutable if the pointer is accessible as mutable.
unsafe impl Send for ThreadKernelStack {}
// SAFETY: See above.
unsafe impl Sync for ThreadKernelStack {}

impl Drop for ThreadKernelStack {
    fn drop(&mut self) {
        let mut pma = PMAlloc::get();
        // SAFETY: By invariants.
        unsafe {
            pma.deallocate(
                self.inner.cast(),
                kalloc::phys::what_order(THREAD_STACK_SIZE),
            );
        }
    }
}

// TODO: guard page
#[cfg(debug_assertions)]
pub const THREAD_STACK_SIZE: usize = 4 * units::MIB;
#[cfg(not(debug_assertions))]
pub const THREAD_STACK_SIZE: usize = 512 * units::KIB;

pub const TRAPFRAME_ADDR: usize = usize::MAX - 3 * 4 * units::KIB + 1;

impl<'a> SlotRef<'a, NotificationCap> {
    pub fn word(&self) -> &AtomicU64 {
        // SAFETY: Type check.
        unsafe { &self.cap.notification().unwrap_unchecked().word }
    }
}

impl<'a, C: Capability> SlotRef<'a, C> {
    pub fn badge(&self) -> Option<NonZeroU64> {
        self.badge
    }
}

#[derive(Debug)]
pub struct Notification {
    pub word: AtomicU64,
    pub wait_queue: Arc<SpinMutex<WaitQueue>>,
}

#[derive(Debug)]
pub struct InterruptPool {
    pub map: SpinRwLock<BTreeMap<u16, Arc<InterruptHandler>>>,
}

#[derive(Debug)]
pub struct InterruptHandler {
    pub notification: SpinMutex<Option<(Arc<Notification>, u64)>>,
    pub irq: u16,
}

impl CapabilityValue for Arc<InterruptPool> {
    type Cap = InterruptPoolCap;

    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::InterruptPool(self)
    }
}

impl CapabilityValue for Arc<InterruptHandler> {
    type Cap = InterruptHandlerCap;

    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::InterruptHandler(self)
    }
}

impl<'a> SlotRefMut<'a, InterruptPoolCap> {
    /// Create an interrupt handler from the pool, with the given IRQ
    /// number.
    ///
    /// # Errors
    ///
    /// - [`CapError::InvalidOperation`]: The IRQ number was already
    ///   allocated from the pool.
    pub fn make_handler(&mut self, irq: u16) -> CapResult<Arc<InterruptHandler>> {
        let handler = Arc::new(InterruptHandler {
            notification: SpinMutex::new(None),
            irq,
        });
        // SAFETY: Type check.
        let pool = unsafe { self.cap.intr_pool().unwrap_unchecked() };
        if pool.map.read().contains_key(&irq) {
            return Err(CapError::InvalidOperation);
        }
        pool.map.write().insert(irq, handler.clone());
        PLIC.set_priority(irq as u32, 1);
        PLIC.hart_senable(irq as u32);
        Ok(handler)
    }
}

static GLOBAL_POOL: OnceCell<Arc<InterruptPool>> = OnceCell::new();

pub fn global_interrupt_pool() -> &'static Arc<InterruptPool> {
    GLOBAL_POOL.get_or_init(|| {
        Arc::new(InterruptPool {
            map: SpinRwLock::new(BTreeMap::new()),
        })
    })
}

#[derive(Debug)]
pub struct Endpoint {
    pub recv_wait_queue: Arc<SpinMutex<WaitQueue>>,
    pub send_wait_queue: Arc<SpinMutex<WaitQueue>>,
    pub reply_cap: SpinMutex<Option<Arc<Endpoint>>>,
}

impl Endpoint {
    //#[track_caller]
    pub fn block_senders(
        &self,
        thread: &Arc<Thread>,
        private: &mut ThreadProtected,
        send_wq: &mut WaitQueue,
    ) {
        thread
            .blocking_wqs
            .lock()
            .push(self.send_wait_queue.clone());
        send_wq.owner.replace(Arc::downgrade(thread));
        let old_prio = private.prio.effective();
        private.prio.inherited_prio = send_wq.highest_prio();
        if thread
            .blocked_on_wq
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            thread.wait_queue_priority_changed(old_prio, private.prio.effective(), true);
        }
    }

    pub fn unblock_senders(
        &self,
        thread: &Arc<Thread>,
        private: &mut ThreadProtected,
        send_wq: &mut WaitQueue,
    ) {
        thread
            .blocking_wqs
            .lock()
            .retain(|x| Arc::ptr_eq(x, &self.send_wait_queue));
        send_wq.owner.take();
        let old_prio = private.prio.effective();
        private.prio.inherited_prio = -1;
        if thread
            .blocked_on_wq
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            thread.wait_queue_priority_changed(old_prio, private.prio.effective(), true);
        }
    }
}

impl CapabilityValue for Arc<Endpoint> {
    type Cap = EndpointCap;

    fn into_kind(self) -> CapabilityKind {
        CapabilityKind::Endpoint(self)
    }
}

mod impls {
    use alloc::sync::Arc;
    use rille::capability::CapabilityType;

    use crate::paging::SharedPageTable;

    use super::{
        AnyCap, AnyCapVal, Capability, CaptblCap, Empty, EmptyCap, Endpoint, EndpointCap,
        InterruptHandler, InterruptHandlerCap, InterruptPool, InterruptPoolCap, Notification,
        NotificationCap, PageCap, PageTableCap, ThreadCap, WeakCaptbl,
    };
    impl Capability for EmptyCap {
        type Value = Empty;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Empty
        }
    }
    impl Capability for CaptblCap {
        type Value = WeakCaptbl;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Captbl
        }
    }
    impl Capability for PageTableCap {
        type Value = SharedPageTable;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::PgTbl
        }
    }
    impl Capability for PageCap {
        type Value = super::Page;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Page
        }
    }
    impl Capability for ThreadCap {
        type Value = Arc<super::Thread>;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Thread
        }
    }
    impl Capability for NotificationCap {
        type Value = Arc<Notification>;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Notification
        }
    }
    impl Capability for AnyCap {
        type Value = AnyCapVal;
        fn is_valid_type(_: CapabilityType) -> bool {
            true
        }
    }
    impl Capability for InterruptHandlerCap {
        type Value = Arc<InterruptHandler>;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::InterruptHandler
        }
    }

    impl Capability for InterruptPoolCap {
        type Value = Arc<InterruptPool>;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::InterruptPool
        }
    }

    impl Capability for EndpointCap {
        type Value = Arc<Endpoint>;
        fn is_valid_type(cap_type: CapabilityType) -> bool {
            cap_type == CapabilityType::Endpoint
        }
    }
}
