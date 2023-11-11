use core::fmt;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::NonNull;

use bitfield::bitfield;
use itertools::Itertools;
use rille::capability::paging::{BasePage, GigaPage, MegaPage, PageTableFlags, PagingLevel};
use rille::capability::{CapError, CapResult, CapabilityType};

use rille::addr::{canonicalize, DirectMapped, Physical, PhysicalMut, Ppn, VirtualMut, Vpn, Vpns};

use crate::paging::{self, RawPageTable};
use crate::sync::{SpinRwLock, SpinRwLockWriteGuard};

use self::captbl::{Captbl, CaptblSlot, WeakCaptbl};
use self::derivation::DerivationTreeNode;
use self::slotref::{SlotRef, SlotRefMut};
//use self::untyped::{CapMetaRef, Untyped, UntypedSlot};

pub mod captbl;
pub mod derivation;
pub mod slotref;
pub mod untyped;

#[repr(C)]
pub union RawCapability {
    pub empty: EmptySlot,
    pub captbl: ManuallyDrop<CaptblSlot>,
    pub allocator: AllocatorSlot,
    //pub untyped: UntypedSlot,
    pub pgtbl: PgTblSlot,
    pub page: PageSlot,
    raw: [u8; 16],
}

impl Default for RawCapability {
    fn default() -> Self {
        Self { empty: EmptySlot }
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct CapabilitySlot {
    lock: SpinRwLock<RawCapabilitySlot>,
}

#[derive(Default, PartialEq)]
#[repr(C, align(8))]
pub struct RawCapabilitySlot {
    pub cap: RawCapability,
    #[allow(clippy::missing_fields_in_debug)]
    pub cap_type: CapabilityType,
    pub dtnode: DerivationTreeNode,
}

#[derive(Copy, Clone, Debug)]
#[repr(align(64))]
pub struct CaptblHeader {
    //refcount: AtomicU64,
    //captbl_lock: SpinRwLock<()>,
    n_slots_log2: u8,
}

const _ASSERT_CAPTBLHEADER_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CaptblHeader>() == 64,
    "CaptblHeader size was not 64 bytes"
);

const _ASSERT_RAWCAPSLOT_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CapabilitySlot>() == 64,
    "RawCapabilitySlot size was not 64 bytes"
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
        let slots = unsafe { &**self.inner };

        let slot = slots.get(index).ok_or(CapError::NotPresent)?;
        // SAFETY: By invariants.
        let slot = unsafe { &*slot.slot };
        let guard = slot.lock.read();

        if !C::is_valid_type(guard.cap_type) {
            return Err(CapError::InvalidType);
        }

        let meta = C::metadata_from_slot(&guard);
        Ok(SlotRef {
            slot: guard,
            meta,
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
    pub fn get_mut<C: Capability>(&self, index: usize) -> CapResult<SlotRefMut<'_, C>> {
        if index == 0 {
            return Err(CapError::NotPresent);
        }

        // SAFETY: By invariants.
        let slots = unsafe { &**self.inner };

        let slot = slots.get(index).ok_or(CapError::NotPresent)?;
        // SAFETY: By invariants.
        let slot = unsafe { &*slot.slot };
        let guard = slot.lock.write();

        if !C::is_valid_type(guard.cap_type) {
            return Err(CapError::InvalidType);
        }

        let meta = C::metadata_from_slot(&guard);
        Ok(SlotRefMut {
            slot: guard,
            meta,
            _phantom: PhantomData,
        })
    }

    // pub fn get_mut<C: Capability>(&self, index: usize) -> CapResult<SlotRefMut<'_, C>> {
    // }
}

pub trait CapToOwned {
    type Target;

    fn to_owned_cap(&self) -> Self::Target;
}

pub trait Capability
where
    Self: Sized,
{
    type Metadata: fmt::Debug;

    fn is_valid_type(cap_type: CapabilityType) -> bool;

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata;

    fn into_meta(self) -> Self::Metadata;

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability);

    /// Perform any potential deallocation and refcounting when this
    /// capability slot is deleted.
    ///
    /// # Safety
    ///
    /// This function must be called only once per slot.
    unsafe fn do_delete(meta: Self::Metadata, slot: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>);
}

impl Capability for EmptySlot {
    type Metadata = ();

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Empty
    }

    fn into_meta(self) -> Self::Metadata {}
    fn metadata_from_slot(_: &RawCapabilitySlot) {}
    fn metadata_to_slot(_: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Empty,
            RawCapability {
                empty: EmptySlot::default(),
            },
        )
    }
    unsafe fn do_delete(_: Self::Metadata, _: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {}
}

impl<'a> CapToOwned for SlotRef<'a, EmptySlot> {
    type Target = EmptySlot;

    fn to_owned_cap(&self) -> Self::Target {
        EmptySlot::default()
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, EmptySlot> {
    type Target = EmptySlot;

    fn to_owned_cap(&self) -> Self::Target {
        EmptySlot::default()
    }
}

#[derive(Clone, Debug)]
pub enum AnyCap {
    Empty,
    Captbl(WeakCaptbl),
    // Untyped(Untyped),
    Allocator,
    Page(Page),
    PgTbl(PgTbl),
    // TODO
}

impl AnyCap {
    pub fn cap_type(&self) -> CapabilityType {
        match self {
            AnyCap::Empty => CapabilityType::Empty,
            AnyCap::Captbl(_) => CapabilityType::Captbl,
            // AnyCap::Untyped(_) => CapabilityType::Untyped,
            AnyCap::Allocator => CapabilityType::Allocator,
            AnyCap::Page(_) => CapabilityType::Page,
            AnyCap::PgTbl(_) => CapabilityType::PgTbl,
        }
    }
}

impl Capability for AnyCap {
    type Metadata = AnyCap;

    #[inline]
    fn is_valid_type(_: CapabilityType) -> bool {
        true
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        match slot.cap_type {
            CapabilityType::Empty => AnyCap::Empty,
            CapabilityType::Captbl => AnyCap::Captbl(Captbl::metadata_from_slot(slot)),
            CapabilityType::Allocator => AnyCap::Allocator,
            //CapabilityType::Untyped => AnyCap::Untyped(Untyped::metadata_from_slot(slot)),
            CapabilityType::PgTbl => AnyCap::PgTbl(PgTbl::metadata_from_slot(slot)),
            CapabilityType::Page => AnyCap::Page(Page::metadata_from_slot(slot)),
            CapabilityType::Unknown => todo!(),
        }
    }

    fn into_meta(self) -> Self::Metadata {
        self
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        match meta {
            AnyCap::Empty => EmptySlot::metadata_to_slot(&()),
            AnyCap::Captbl(tbl) => Captbl::metadata_to_slot(tbl),
            // AnyCap::Untyped(ut) => Untyped::metadata_to_slot(ut),
            AnyCap::Allocator => AllocatorSlot::metadata_to_slot(&()),
            AnyCap::Page(page) => Page::metadata_to_slot(page),
            AnyCap::PgTbl(pgtbl) => PgTbl::metadata_to_slot(pgtbl),
        }
    }

    unsafe fn do_delete(meta: Self::Metadata, slot: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        // SAFETY: Our invariants ensure this isn't called twice, and
        // we downcast correctly.
        unsafe {
            match meta {
                AnyCap::Empty => {}
                AnyCap::Captbl(tbl) => Captbl::do_delete(tbl, slot),
                // AnyCap::Untyped(ut) => Untyped::do_delete(ut, slot),
                AnyCap::Allocator => AllocatorSlot::do_delete((), slot),
                AnyCap::Page(pg) => Page::do_delete(pg, slot),
                AnyCap::PgTbl(pgtbl) => PgTbl::do_delete(pgtbl, slot),
            }
        }
    }
}

impl<'a> CapToOwned for SlotRef<'a, AnyCap> {
    type Target = AnyCap;

    fn to_owned_cap(&self) -> AnyCap {
        match &self.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Allocator => AnyCap::Allocator,
            // AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
            AnyCap::Page(page) => AnyCap::Page(page.clone()),
            AnyCap::PgTbl(pgtbl) => AnyCap::PgTbl(pgtbl.clone()),
        }
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, AnyCap> {
    type Target = AnyCap;

    fn to_owned_cap(&self) -> AnyCap {
        match &self.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Allocator => AnyCap::Allocator,
            // AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
            AnyCap::Page(page) => AnyCap::Page(page.clone()),
            AnyCap::PgTbl(pgtbl) => AnyCap::PgTbl(pgtbl.clone()),
        }
    }
}

impl<'a> SlotRefMut<'a, EmptySlot> {
    pub fn replace<C: Capability>(self, val: C) -> SlotRefMut<'a, C> {
        let mut slot = SlotRefMut {
            slot: self.slot,
            meta: val.into_meta(),
            _phantom: PhantomData,
        };
        (slot.slot.cap_type, slot.slot.cap) = C::metadata_to_slot(&slot.meta);
        slot
    }
}

impl RawCapability {
    pub fn captbl(slot: CaptblSlot) -> Self {
        RawCapability {
            captbl: ManuallyDrop::new(slot),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct EmptySlot;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageSlot {
    pub ppn: Ppn,
    pub size_log2: u8,
}

#[derive(Clone, Debug)]
pub struct Page {
    phys: PhysicalMut<u8, DirectMapped>,
    size_log2: u8,
}

impl Page {
    pub unsafe fn new(phys: PhysicalMut<u8, DirectMapped>, size_log2: u8) -> Self {
        Self { phys, size_log2 }
    }
}

impl Capability for Page {
    type Metadata = Page;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Page
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants.
        let slot = unsafe { slot.cap.page };
        Page {
            phys: Physical::from_components(slot.ppn, None),
            size_log2: slot.size_log2,
        }
    }

    fn into_meta(self) -> Self::Metadata {
        self
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Page,
            RawCapability {
                page: PageSlot {
                    ppn: meta.phys.ppn(),
                    size_log2: meta.size_log2,
                },
            },
        )
    }

    unsafe fn do_delete(meta: Self::Metadata, slot: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        // if let Some(untyped) = meta.untyped {
        //     // SAFETY: If we're valid and this is non-null, it must be
        //     // valid.
        //     let ut = unsafe { untyped.as_ref() };
        //     let ut = ut.lock.write();
        //     let ut_meta = Untyped::metadata_from_slot(&ut.cap);
        //     let mut ut: SlotRefMut<Untyped> = SlotRefMut {
        //         slot: ut,
        //         meta: ut_meta,
        //         _phantom: PhantomData,
        //     };
        //     // SAFETY: By our invariants.
        //     unsafe { ut.handle_child_deletion(CapMetaRef::Page(&meta), slot) }
        // }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PgTblSlot {
    pub vpns: Vpns,
    pub page_size_log2: u8,
}

#[derive(Clone)]
pub struct PgTbl {
    table: NonNull<RawPageTable>,
    page_size_log2: u8,
}

impl fmt::Debug for PgTbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgTbl")
            .field("<ptr>", &self.table.as_ptr())
            // SAFETY: table is modified atomically
            .field("tbl", unsafe { self.table.as_ref() })
            .finish()
    }
}

impl PgTbl {
    pub unsafe fn new(table: NonNull<RawPageTable>, page_size_log2: u8) -> Self {
        Self {
            table,
            page_size_log2,
        }
    }
}

impl Capability for PgTbl {
    type Metadata = PgTbl;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::PgTbl
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants.
        let slot = unsafe { slot.cap.pgtbl };
        PgTbl {
            // SAFETY: By invariants
            table: unsafe {
                NonNull::new_unchecked(
                    VirtualMut::<_, DirectMapped>::from_components(slot.vpns.0, None)
                        .into_ptr_mut(),
                )
            },
            page_size_log2: slot.page_size_log2,
        }
    }

    fn into_meta(self) -> Self::Metadata {
        self
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::PgTbl,
            RawCapability {
                pgtbl: PgTblSlot {
                    vpns: Vpns(VirtualMut::<_, DirectMapped>::from_ptr(meta.table.as_ptr()).vpns()),
                    page_size_log2: meta.page_size_log2,
                },
            },
        )
    }

    unsafe fn do_delete(meta: Self::Metadata, slot: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        // if let Some(untyped) = meta.untyped {
        //     // SAFETY: If we're valid and this is non-null, it must be
        //     // valid.
        //     let ut = unsafe { untyped.as_ref() };
        //     let ut = ut.lock.write();
        //     let ut_meta = Untyped::metadata_from_slot(&ut.cap);
        //     let mut ut: SlotRefMut<Untyped> = SlotRefMut {
        //         slot: ut,
        //         meta: ut_meta,
        //         _phantom: PhantomData,
        //     };
        //     // SAFETY: By our invariants.
        //     unsafe { ut.handle_child_deletion(CapMetaRef::PgTbl(&meta), slot) }
        // }
    }
}

impl<'a> SlotRefMut<'a, PgTbl> {
    pub fn map_page(
        &mut self,
        page: &mut SlotRefMut<'_, Page>,
        vpn: Vpn,
        flags: PageTableFlags,
    ) -> CapResult<()> {
        use paging::PageTableFlags as KernelFlags;

        if page.meta.size_log2 != self.meta.page_size_log2 {
            return Err(CapError::InvalidSize);
        }

        if page.meta.size_log2 == GigaPage::PAGE_SIZE_LOG2 as u8 && vpn.into_u16() >= 128 {
            return Err(CapError::InvalidOperation);
        }

        let flags = KernelFlags::from_bits_truncate(flags.bits()) | KernelFlags::VAD;

        // // SAFETY: This table is synchronized atomically.
        // let table = unsafe { self.meta.table.as_ref() };
        // let entry = &table.ptes[vpn.into_usize()];
        // entry
        // entry.store(
        //     PageTableEntry {
        //         flags,
        //         ppn: page.meta.phys.ppn(),
        //     }
        //     .encode()
        //     .0,
        // );
        //        self.add_child(page);

        todo!();

        Ok(())
    }

    pub fn map_pgtbl(
        &mut self,
        pgtbl: &mut SlotRefMut<'_, PgTbl>,
        vpn: Vpn,
        _flags: PageTableFlags,
    ) -> CapResult<()> {
        use paging::PageTableFlags as KernelFlags;

        let self_size = self.meta.page_size_log2 as usize;
        let other_size = pgtbl.meta.page_size_log2 as usize;
        if self_size == BasePage::PAGE_SIZE_LOG2
            || (self_size == MegaPage::PAGE_SIZE_LOG2 && other_size != BasePage::PAGE_SIZE_LOG2)
            || (self_size == GigaPage::PAGE_SIZE_LOG2 && other_size != MegaPage::PAGE_SIZE_LOG2)
        {
            return Err(CapError::InvalidSize);
        }

        if self_size == GigaPage::PAGE_SIZE_LOG2 && vpn.into_u16() >= 128 {
            return Err(CapError::InvalidOperation);
        }

        let flags = KernelFlags::VAD;

        // // SAFETY: This table is synchronized atomically.
        // let table = unsafe { self.meta.table.as_ref() };
        // let entry = &table.ptes[vpn.into_usize()];
        // entry.store(
        //     PageTableEntry {
        //         flags,
        //         ppn: VirtualConst::<_, DirectMapped>::from_ptr(pgtbl.meta.table.as_ptr())
        //             .into_phys()
        //             .ppn(),
        //     }
        //     .encode()
        //     .0,
        //     Ordering::Release,
        // );
        // TODO: sfence
        todo!();

        Ok(())
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AllocatorSlot;

impl Capability for AllocatorSlot {
    type Metadata = ();

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Allocator
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) {}

    fn into_meta(self) {}

    fn metadata_to_slot(meta: &()) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Allocator,
            RawCapability {
                allocator: AllocatorSlot::default(),
            },
        )
    }

    unsafe fn do_delete(meta: Self::Metadata, slot: &SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        todo!()
    }
}

/*






*/

impl PartialEq for RawCapability {
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: as_bits covers the entire union and any bitpattern
        // for it is valid.
        unsafe { self.raw == other.raw }
    }
}

impl Eq for RawCapability {}

// TODO
impl fmt::Debug for RawCapabilitySlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawCapabilitySlot")
            .field("cap", &RawCapDebugAdapter(&self))
            .field("dtnode", &self.dtnode)
            .finish()
    }
}

struct RawCapDebugAdapter<'a>(&'a RawCapabilitySlot);

impl<'a> fmt::Debug for RawCapDebugAdapter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.cap_type {
            CapabilityType::Empty => f.debug_struct("EmptySlot").finish(),
            // SAFETY: Type check.
            CapabilityType::Captbl => unsafe { &self.0.cap.captbl }.fmt(f),
            // SAFETY: Type check.
            CapabilityType::Allocator => unsafe { &self.0.cap.allocator }.fmt(f),
            // // SAFETY: Type check.
            // CapabilityType::Untyped => unsafe { &self.untyped }.fmt(f),
            // SAFETY: Type check.
            CapabilityType::PgTbl => unsafe { &self.0.cap.pgtbl }.fmt(f),
            // SAFETY: Type check.
            CapabilityType::Page => unsafe { &self.0.cap.page }.fmt(f),
            CapabilityType::Unknown => f.debug_struct("InvalidCap").finish(),
        }
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct SlotPtrWithTable(u64);
    /// We only need 35 bits (39 - log2(64)) due to alignment.
    u64, slot_addr, set_slot_addr: 32, 0;
    /// We only need 27 bits (39 - log2(4096)) due to page alignment.
    u64, captbl_addr, set_captbl_addr: 59, 33;
}

impl fmt::Debug for SlotPtrWithTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self == &SlotPtrWithTable::null() {
            write!(f, "<null>")
        } else {
            f.debug_struct("SlotPtrWithTable")
                .field("slot", &self.slot())
                .field("captbl", &self.captbl())
                .finish()
        }
    }
}

impl SlotPtrWithTable {
    pub const NULL: Self = Self::null();

    pub const fn null() -> Self {
        Self(0)
    }

    pub fn new(slot: *const CapabilitySlot, captbl: *const CaptblHeader) -> Self {
        Self(0).with_slot(slot).with_captbl(captbl)
    }

    pub fn slot(&self) -> *const CapabilitySlot {
        canonicalize((self.slot_addr() << 6) as usize) as *const _
    }

    #[must_use]
    pub fn with_slot(mut self, slot: *const CapabilitySlot) -> Self {
        self.set_slot_addr((slot as usize >> 6) as u64);
        self
    }

    pub fn captbl(&self) -> *const CaptblHeader {
        canonicalize((self.captbl_addr() << 12) as usize) as *const _
    }

    #[must_use]
    pub fn with_captbl(mut self, captbl: *const CaptblHeader) -> Self {
        self.set_captbl_addr((captbl as usize >> 12) as u64);
        self
    }
}

// #[repr(C, align(4096))]
// pub struct Thread {
//     trapframe: Trapframe,
//     captbl: Option<Captbl>,
//     root_pgtbl: Option<NonNull<()>>,
//     state: ThreadState,
//     context: Context,
//     stack: ThreadKernelStack,
// }

// #[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
// #[repr(u8)]
// pub enum ThreadState {
//     #[num_enum(default)]
//     Uninit = 0,
//     Suspended = 1,
//     Blocking = 2,
//     Runnable = 3,
//     Running = 4,
// }

// #[repr(C, align(4096))]
// pub struct ThreadKernelStack {
//     inner: [u8; THREAD_STACK_SIZE],
// }

// pub const THREAD_STACK_SIZE: usize = 128 * units::KIB;
