use core::fmt;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::{self, NonNull};
use core::sync::atomic::AtomicU64;

use bitfield::bitfield;
use itertools::Itertools;
use rille::capability::{CapError, CapResult, CapabilityType};

use rille::addr::{canonicalize, Ppn, Vpns};

use crate::sync::SpinRwLock;

use self::captbl::{Captbl, CaptblSlot};
use self::derivation::DerivationTreeNode;
use self::slotref::{SlotRef, SlotRefMut};
use self::untyped::{Untyped, UntypedSlot};

pub mod captbl;
pub mod derivation;
pub mod slotref;
pub mod untyped;

#[repr(C)]
pub union RawCapability {
    pub empty: EmptySlot,
    captbl: ManuallyDrop<CaptblSlot>,
    pub untyped: UntypedSlot,
    pub pgtbl: PgTblSlot,
    pub page: PageSlot,
    pub raw: [u8; 16],
}

#[repr(C)]
#[derive(Debug)]
pub struct CapabilitySlot {
    lock: SpinRwLock<RawCapabilitySlot>,
}

#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct RawCapabilitySlot {
    pub cap: RawCapability,
    pub dtnode: DerivationTreeNode,
}

#[derive(Debug)]
#[repr(align(64))]
pub struct CaptblHeader {
    refcount: AtomicU64,
    //captbl_lock: SpinRwLock<()>,
    n_slots_log2: u8,
    untyped: Option<NonNull<CapabilitySlot>>,
}

const _ASSERT_CAPTBLHEADER_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CaptblHeader>() == 64,
    "CaptblHeader size was not 64 bytes"
);

const _ASSERT_RAWCAPSLOT_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CapabilitySlot>() == 64,
    "RawCapabilitySlot size was not 64 bytes"
);

#[repr(transparent)]
pub struct CaptblSlots {
    inner_no_meta: [CapabilitySlot],
}

impl CaptblSlots {
    fn ptr_from_raw(ptr: *const [CapabilitySlot]) -> *const Self {
        ptr as *const Self
    }
}

impl Captbl {
    fn slots(&self) -> &CaptblSlots {
        let slots_base_ptr = self
            .hdr
            .as_ptr()
            .wrapping_add(1)
            .cast::<CapabilitySlot>()
            .cast_const();
        let slots_slice_ptr =
            ptr::slice_from_raw_parts(slots_base_ptr, (1 << self.n_slots_log2) - 1);

        // SAFETY: Our invariants ensure this is valid.
        unsafe { &*CaptblSlots::ptr_from_raw(slots_slice_ptr) }
    }

    unsafe fn slots_uninit(&mut self) -> &mut [MaybeUninit<CapabilitySlot>] {
        let slots_base_ptr = self
            .hdr
            .as_ptr()
            .wrapping_add(1)
            .cast::<MaybeUninit<CapabilitySlot>>();
        let slots_slice_ptr =
            ptr::slice_from_raw_parts_mut(slots_base_ptr, (1 << self.n_slots_log2) - 1);

        // SAFETY: By invariants.
        unsafe { &mut *slots_slice_ptr }
    }

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

        let slot = self
            .slots()
            .inner_no_meta
            .get(index - 1)
            .ok_or(CapError::NotPresent)?;
        let guard = slot.lock.read();

        if !C::is_slot_valid_type(&guard.cap) {
            return Err(CapError::InvalidType);
        }

        let meta = C::metadata_from_slot(&guard.cap);
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

        let slot = self
            .slots()
            .inner_no_meta
            .get(index - 1)
            .ok_or(CapError::NotPresent)?;
        let guard = slot.lock.write();

        if !C::is_slot_valid_type(&guard.cap) {
            return Err(CapError::InvalidType);
        }

        let meta = C::metadata_from_slot(&guard.cap);
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

    fn is_slot_valid_type(slot: &RawCapability) -> bool;

    fn metadata_from_slot(slot: &RawCapability) -> Self::Metadata;

    fn into_meta(self) -> Self::Metadata;

    fn metadata_to_slot(meta: &Self::Metadata) -> RawCapability;

    /// Perform any potential deallocation and refcounting when this
    /// capability slot is deleted.
    ///
    /// # Safety
    ///
    /// This function must be called only once per slot.
    unsafe fn do_delete(slot: SlotRefMut<'_, Self>);
}

impl Capability for EmptySlot {
    type Metadata = ();

    fn is_slot_valid_type(slot: &RawCapability) -> bool {
        slot.cap_type() == CapabilityType::Empty
    }

    fn into_meta(self) -> Self::Metadata {}
    fn metadata_from_slot(_: &RawCapability) {}
    fn metadata_to_slot(_: &Self::Metadata) -> RawCapability {
        RawCapability {
            empty: EmptySlot::default(),
        }
    }
    unsafe fn do_delete(_: SlotRefMut<'_, Self>) {}
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
    Captbl(ManuallyDrop<Captbl>),
    Untyped(Untyped),
    // TODO
}

impl AnyCap {
    pub fn cap_type(&self) -> CapabilityType {
        match self {
            AnyCap::Empty => CapabilityType::Empty,
            AnyCap::Captbl(_) => CapabilityType::Unknown,
            AnyCap::Untyped(_) => CapabilityType::Untyped,
        }
    }
}

impl Capability for AnyCap {
    type Metadata = AnyCap;

    #[inline]
    fn is_slot_valid_type(_: &RawCapability) -> bool {
        true
    }

    fn metadata_from_slot(slot: &RawCapability) -> Self::Metadata {
        match slot.cap_type() {
            CapabilityType::Empty => AnyCap::Empty,
            CapabilityType::Captbl => AnyCap::Captbl(Captbl::metadata_from_slot(slot)),
            CapabilityType::Untyped => AnyCap::Untyped(Untyped::metadata_from_slot(slot)),
            CapabilityType::PgTbl => todo!(),
            CapabilityType::Page => todo!(),
            CapabilityType::Unknown => todo!(),
        }
    }

    fn into_meta(self) -> Self::Metadata {
        self
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> RawCapability {
        match meta {
            AnyCap::Empty => EmptySlot::metadata_to_slot(&()),
            AnyCap::Captbl(tbl) => Captbl::metadata_to_slot(tbl),
            AnyCap::Untyped(ut) => Untyped::metadata_to_slot(ut),
        }
    }

    unsafe fn do_delete(slot: SlotRefMut<'_, Self>) {
        // SAFETY: Our invariants ensure this isn't called twice, and
        // we downcast correctly.
        unsafe {
            match slot.meta.cap_type() {
                CapabilityType::Empty => {}
                CapabilityType::Captbl => Captbl::do_delete(slot.downcast_mut().unwrap()),
                CapabilityType::Untyped => Untyped::do_delete(slot.downcast_mut().unwrap()),
                CapabilityType::PgTbl => todo!(),
                CapabilityType::Page => todo!(),
                CapabilityType::Unknown => todo!(),
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
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
        }
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, AnyCap> {
    type Target = AnyCap;

    fn to_owned_cap(&self) -> AnyCap {
        match &self.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
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
        slot.slot.cap = C::metadata_to_slot(&slot.meta);
        slot
    }
}

impl RawCapability {
    #[inline]
    pub fn cap_type(&self) -> CapabilityType {
        // SAFETY: cap_type is always the same size and at the same
        // offset.
        unsafe { self.empty.cap_type() }
    }

    pub fn captbl(slot: CaptblSlot) -> Self {
        RawCapability {
            captbl: ManuallyDrop::new(slot),
        }
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct EmptySlot(u128);
    impl Debug;
    u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
}

impl EmptySlot {
    fn with_cap_type(mut self, cap_type: CapabilityType) -> Self {
        self.set_cap_type(cap_type);
        self
    }
}

impl Default for EmptySlot {
    fn default() -> Self {
        EmptySlot(0).with_cap_type(CapabilityType::Empty)
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct PageSlot(u128);
    impl Debug;
    u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u64, from into Ppn, phys_addr, set_phys_addr: 31, 5;
    pub u8, size_log2, set_size_log2: 36, 32;
}

impl PageSlot {
    fn with_cap_type(mut self, cap_type: CapabilityType) -> Self {
        self.set_cap_type(cap_type);
        self
    }

    #[must_use]
    pub fn with_phys_addr(mut self, phys_addr: Ppn) -> Self {
        self.set_phys_addr(phys_addr);
        self
    }

    #[must_use]
    pub fn with_size_log2(mut self, size_log2: u8) -> Self {
        self.set_size_log2(size_log2);
        self
    }
}

impl Default for PageSlot {
    fn default() -> Self {
        PageSlot(0).with_cap_type(CapabilityType::Page)
    }
}

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct PgTblSlot(u128);
    impl Debug;
    u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    pub u32, from into Vpns, virt_addr, set_virt_addr: 31, 5;
}

impl PgTblSlot {
    fn with_cap_type(mut self, cap_type: CapabilityType) -> Self {
        self.set_cap_type(cap_type);
        self
    }

    #[must_use]
    pub fn with_virt_addr(mut self, virt_addr: Vpns) -> Self {
        self.set_virt_addr(virt_addr);
        self
    }
}

impl Default for PgTblSlot {
    fn default() -> Self {
        PgTblSlot(0).with_cap_type(CapabilityType::PgTbl)
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

impl fmt::Debug for RawCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cap_type() {
            CapabilityType::Empty => f.debug_struct("EmptySlot").finish(),
            // SAFETY: Type check.
            CapabilityType::Captbl => unsafe { &self.captbl }.fmt(f),
            // SAFETY: Type check.
            CapabilityType::Untyped => unsafe { &self.untyped }.fmt(f),
            CapabilityType::PgTbl => todo!(),
            CapabilityType::Page => todo!(),
            CapabilityType::Unknown => f.debug_struct("InvalidCap").finish(),
        }
    }
}

struct DebugAdapterCoalesce<'a>(u64, &'a CapabilitySlot);

impl<'a> fmt::Debug for DebugAdapterCoalesce<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.1.fmt(f)?;
        match self.0 {
            0 => Ok(()),
            n => write!(f, " <repeated {} times>", n + 1),
        }
    }
}

impl fmt::Debug for CaptblSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.inner_no_meta
                    .iter()
                    .map(|x| DebugAdapterCoalesce(0, x))
                    .coalesce(|l, r| {
                        if *l.1.lock.read() == *r.1.lock.read() {
                            Ok(DebugAdapterCoalesce(l.0 + 1, l.1))
                        } else {
                            Err((l, r))
                        }
                    })
                    .map(|x| (x.1 as *const _, x)),
            )
            .finish()
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
// pub struct ThreadControlBlock {
//     trapframe: Trapframe,
//     captbl: Option<Captbl>,
//     root_pgtbl: Option<NonNull<()>>,
//     state: ThreadState,
//     context: Context,
//     stack: TCBKernelStack,
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
// pub struct TCBKernelStack {
//     inner: [u8; THREAD_STACK_SIZE],
// }

// pub const THREAD_STACK_SIZE: usize = 128 * units::KIB;
