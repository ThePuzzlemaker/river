use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::num::NonZeroUsize;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU64, Ordering};
use core::{fmt, mem};

use bitfield::bitfield;
use itertools::Itertools;
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use rille::capability::CapabilityType;

use rille::addr::{
    canonicalize, decanonicalize, DirectMapped, PgOff, Physical, PhysicalMut, Ppn, Virtual,
    VirtualConst, VirtualMut, Vpns,
};
use rille::units::{self, StorageUnits};

use crate::kalloc;
use crate::proc::Context;
use crate::sync::{MappedSpinRwLockReadGuard, MappedSpinRwLockWriteGuard, SpinRwLock};
use crate::trampoline::Trapframe;

use self::captbl::{Captbl, CaptblSlot};

pub mod captbl;
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
pub struct RawCapabilitySlot {
    pub cap: RawCapability,
    pub dtnode: DerivationTreeNode,
}

#[derive(Debug)]
#[repr(align(64))]
pub struct CaptblHeader {
    refcount: AtomicU64,
    captbl_lock: SpinRwLock<()>,
    n_slots_log2: u8,
    untyped: usize,
}

const _ASSERT_NULLCAPSLOT_SIZE_EQ_64: () = assert!(
    core::mem::size_of::<CaptblHeader>() == 64,
    "NullCapSlot size was not 64 bytes"
);

pub type CaptblReadGuard<'a> = MappedSpinRwLockReadGuard<'a, (), CaptblSlots>;
pub type CaptblWriteGuard<'a> = MappedSpinRwLockWriteGuard<'a, (), CaptblSlots>;

impl CaptblHeader {
    #[inline]
    pub fn try_read(&self) -> Option<CaptblReadGuard<'_>> {
        Some(self.captbl_lock.try_read()?.map(|_| {
            let slots_base_ptr = ptr::addr_of!(*self)
                .wrapping_add(1)
                .cast::<RawCapabilitySlot>();
            let slots_slice_ptr =
                ptr::slice_from_raw_parts(slots_base_ptr, (1 << self.n_slots_log2) - 1);
            // SAFETY: Our invariants ensure this is
            // valid. Additionally, `map` ensures that this reference
            // only lives when we have the lock.
            unsafe { &*CaptblSlots::ptr_from_raw(slots_slice_ptr) }
        }))
    }

    #[inline]
    pub fn read(&self) -> CaptblReadGuard<'_> {
        self.captbl_lock.read().map(|_| {
            let slots_base_ptr = ptr::addr_of!(*self)
                .wrapping_add(1)
                .cast::<RawCapabilitySlot>();
            let slots_slice_ptr =
                ptr::slice_from_raw_parts(slots_base_ptr, (1 << self.n_slots_log2) - 1);
            // SAFETY: Our invariants ensure this is
            // valid. Additionally, `map` ensures that this reference
            // only lives when we have the lock.
            unsafe { &*CaptblSlots::ptr_from_raw(slots_slice_ptr) }
        })
    }

    #[inline]
    pub fn write(&self) -> MappedSpinRwLockWriteGuard<'_, (), CaptblSlots> {
        self.captbl_lock.write().map(|_| {
            let slots_base_ptr = ptr::addr_of!(*self)
                .wrapping_add(1)
                .cast::<RawCapabilitySlot>()
                .cast_mut();
            let slots_slice_ptr =
                ptr::slice_from_raw_parts_mut(slots_base_ptr, (1 << self.n_slots_log2) - 1);
            // SAFETY: Our invariants ensure this is
            // valid. Additionally, `map` ensures that this reference
            // only lives when we have the lock.
            unsafe { &mut *CaptblSlots::ptr_from_raw_mut(slots_slice_ptr) }
        })
    }
}

#[repr(transparent)]
pub struct CaptblSlots {
    inner_no_meta: [RawCapabilitySlot],
}

impl CaptblSlots {
    fn ptr_from_raw(ptr: *const [RawCapabilitySlot]) -> *const Self {
        ptr as *const Self
    }

    fn ptr_from_raw_mut(ptr: *mut [RawCapabilitySlot]) -> *mut Self {
        ptr as *mut Self
    }

    fn table_addr(&self) -> VirtualConst<CaptblHeader, DirectMapped> {
        VirtualConst::from_ptr(
            (self as *const Self)
                .cast::<RawCapabilitySlot>()
                .wrapping_sub(1)
                .cast(),
        )
    }

    pub fn type_of(&self, ix: usize) -> Option<CapabilityType> {
        if ix == 0 {
            return None;
        }
        Some(self.inner_no_meta.get(ix - 1)?.cap.cap_type())
    }

    pub fn get<C: Capability>(&'_ self, ix: usize) -> Option<SlotRef<C>> {
        if ix == 0 {
            return None;
        }
        let slot = self.inner_no_meta.get(ix - 1)?;
        if !C::is_slot_valid_type(&slot.cap) {
            return None;
        }

        let meta = C::metadata_from_slot(&slot.cap);
        Some(SlotRef {
            slot,
            meta,
            tbl_addr: self.table_addr(),
            _phantom: PhantomData,
        })
    }

    pub fn get_mut<C: Capability>(&'_ mut self, ix: usize) -> Option<SlotRefMut<'_, C>> {
        if ix == 0 {
            return None;
        }
        let tbl_addr = self.table_addr();
        let slot = self.inner_no_meta.get_mut(ix - 1)?;
        if !C::is_slot_valid_type(&slot.cap) {
            return None;
        }

        let meta = C::metadata_from_slot(&slot.cap);
        Some(SlotRefMut {
            slot,
            meta,
            tbl_addr,
            _phantom: PhantomData,
        })
    }

    pub fn get2_mut<C1: Capability, C2: Capability>(
        &'_ mut self,
        ix1: usize,
        ix2: usize,
    ) -> Option<(SlotRefMut<'_, C1>, SlotRefMut<'_, C2>)> {
        if ix1 == 0 || ix2 == 0 || ix1 == ix2 {
            return None;
        }
        let tbl_addr = self.table_addr();
        let [slot1, slot2] = self.inner_no_meta.get_many_mut([ix1 - 1, ix2 - 1]).ok()?;
        if !C1::is_slot_valid_type(&slot1.cap) || !C2::is_slot_valid_type(&slot2.cap) {
            return None;
        }

        let meta1 = C1::metadata_from_slot(&slot1.cap);
        let meta2 = C2::metadata_from_slot(&slot2.cap);
        Some((
            SlotRefMut {
                slot: slot1,
                meta: meta1,
                tbl_addr,
                _phantom: PhantomData,
            },
            SlotRefMut {
                slot: slot2,
                meta: meta2,
                tbl_addr,
                _phantom: PhantomData,
            },
        ))
    }
}

pub struct SlotRef<'a, C: Capability> {
    slot: &'a RawCapabilitySlot,
    tbl_addr: VirtualConst<CaptblHeader, DirectMapped>,
    meta: C::Metadata,
    _phantom: PhantomData<C>,
}

impl<'a, C: Capability> fmt::Debug for SlotRef<'a, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotRef")
            .field("slot", &self.slot)
            .field("meta", &self.meta)
            .field("_phantom", &self._phantom)
            .finish()
    }
}

pub struct SlotRefMut<'a, C: Capability> {
    slot: &'a mut RawCapabilitySlot,
    tbl_addr: VirtualConst<CaptblHeader, DirectMapped>,
    meta: C::Metadata,
    _phantom: PhantomData<C>,
}

impl<'a, C: Capability> fmt::Debug for SlotRefMut<'a, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotRef")
            .field("slot", &self.slot)
            .field("meta", &self.meta)
            .field("_phantom", &self._phantom)
            .finish()
    }
}

pub trait Capability
where
    Self: Sized,
    Self: for<'a> From<SlotRef<'a, Self>>,
{
    type Metadata: fmt::Debug;

    fn is_slot_valid_type(slot: &RawCapability) -> bool;

    fn metadata_from_slot(slot: &RawCapability) -> Self::Metadata;

    fn into_meta(self) -> Self::Metadata;

    fn metadata_to_slot(meta: &Self::Metadata) -> RawCapability;

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

// TODO: make this not From
impl<'a> From<SlotRef<'a, EmptySlot>> for EmptySlot {
    fn from(_: SlotRef<'a, EmptySlot>) -> Self {
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

impl<'a> From<&'_ SlotRef<'a, AnyCap>> for AnyCap {
    fn from(x: &'_ SlotRef<'a, AnyCap>) -> Self {
        match &x.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
        }
    }
}

impl<'a> From<SlotRef<'a, AnyCap>> for AnyCap {
    fn from(x: SlotRef<'a, AnyCap>) -> Self {
        match x.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
        }
    }
}

impl<'a> From<&'_ mut SlotRefMut<'a, AnyCap>> for AnyCap {
    fn from(x: &'_ mut SlotRefMut<'a, AnyCap>) -> Self {
        match &x.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
        }
    }
}

impl<'a> From<SlotRefMut<'a, AnyCap>> for AnyCap {
    fn from(x: SlotRefMut<'a, AnyCap>) -> Self {
        match x.meta {
            AnyCap::Empty => AnyCap::Empty,
            AnyCap::Captbl(tbl) => AnyCap::Captbl(tbl.clone()),
            AnyCap::Untyped(ut) => AnyCap::Untyped(ut.clone()),
        }
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn downgrade(&'_ mut self) -> SlotRef<'_, C> {
        SlotRef {
            slot: self.slot,
            meta: C::metadata_from_slot(&self.slot.cap),
            tbl_addr: self.tbl_addr,
            _phantom: PhantomData,
        }
    }

    pub fn into_const(self) -> SlotRef<'a, C> {
        SlotRef {
            slot: self.slot,
            meta: self.meta,
            tbl_addr: self.tbl_addr,
            _phantom: PhantomData,
        }
    }
}

impl<'a> SlotRef<'a, AnyCap> {
    pub fn downcast<C: Capability>(self) -> Option<SlotRef<'a, C>> {
        if C::is_slot_valid_type(&self.slot.cap) {
            let meta = C::metadata_from_slot(&self.slot.cap);
            Some(SlotRef {
                slot: self.slot,
                meta,
                tbl_addr: self.tbl_addr,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a> SlotRefMut<'a, AnyCap> {
    pub fn downcast_mut<C: Capability>(self) -> Option<SlotRefMut<'a, C>> {
        if C::is_slot_valid_type(&self.slot.cap) {
            let meta = C::metadata_from_slot(&self.slot.cap);
            Some(SlotRefMut {
                slot: self.slot,
                meta,
                tbl_addr: self.tbl_addr,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a, C: Capability> SlotRef<'a, C> {
    pub fn upcast(self) -> SlotRef<'a, AnyCap> {
        let meta = AnyCap::metadata_from_slot(&self.slot.cap);
        SlotRef {
            slot: self.slot,
            meta,
            tbl_addr: self.tbl_addr,
            _phantom: PhantomData,
        }
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn upcast_mut(self) -> SlotRefMut<'a, AnyCap> {
        let meta = AnyCap::metadata_from_slot(&self.slot.cap);
        SlotRefMut {
            slot: self.slot,
            meta,
            tbl_addr: self.tbl_addr,
            _phantom: PhantomData,
        }
    }

    pub fn write(&'a mut self, _val: &C) {
        todo!()
    }

    pub fn delete(&mut self) -> SlotRefMut<'a, EmptySlot> {
        todo!()
    }
}

impl<'a> SlotRefMut<'a, EmptySlot> {
    pub fn replace<C: Capability>(&'a mut self, val: C) -> SlotRefMut<'a, C> {
        let slot = SlotRefMut {
            slot: self.slot,
            meta: val.into_meta(),
            tbl_addr: self.tbl_addr,
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

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct UntypedSlot(u128);
    impl Debug;
    u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    u64, from into Ppn, base_phys_addr, set_base_phys_addr: 31, 5;
    u64, from into Ppn, free_phys_addr, set_free_phys_addr: 58, 32;
    u8, size_log2, set_size_log2: 63, 59;
    // N.B. Untyped memory is not refcounted. It is simply leaked to
    // the init thread, and delegated. It also cannot be retyped or
    // delegated if it has children, and delegated regions prevent
    // retyping of those regions. Thus, it need not be synchronized
    // other than by the captbl lock.
}

impl UntypedSlot {
    fn with_cap_type(mut self, cap_type: CapabilityType) -> Self {
        self.set_cap_type(cap_type);
        self
    }

    #[must_use]
    fn with_base_phys_addr(mut self, phys_addr: Ppn) -> Self {
        self.set_base_phys_addr(phys_addr);
        self
    }

    #[must_use]
    fn with_free_phys_addr(mut self, phys_addr: Ppn) -> Self {
        self.set_free_phys_addr(phys_addr);
        self
    }

    #[must_use]
    fn with_size_log2(mut self, size_log2: u8) -> Self {
        self.set_size_log2(size_log2);
        self
    }
}

impl Default for UntypedSlot {
    fn default() -> Self {
        UntypedSlot(0).with_cap_type(CapabilityType::Untyped)
    }
}

#[derive(Clone, Debug)]
pub struct Untyped {
    base: PhysicalMut<u8, DirectMapped>,
    free: PhysicalMut<u8, DirectMapped>,
    size_log2: u8,
}

impl Untyped {
    /// TODO
    ///
    /// # Safety
    ///
    /// TODO
    pub unsafe fn new(base: PhysicalMut<u8, DirectMapped>, size_log2: u8) -> Self {
        Self {
            base,
            free: base,
            size_log2,
        }
    }
}

impl Capability for Untyped {
    type Metadata = Untyped;

    fn is_slot_valid_type(slot: &RawCapability) -> bool {
        slot.cap_type() == CapabilityType::Untyped
    }

    fn metadata_from_slot(slot: &RawCapability) -> Self::Metadata {
        // SAFETY: By the invariants of this slot, this is safe.
        let slot = unsafe { slot.untyped };
        Untyped {
            base: Physical::from_components(slot.base_phys_addr(), None),
            free: Physical::from_components(slot.free_phys_addr(), None),
            size_log2: slot.size_log2(),
        }
    }

    fn into_meta(self) -> Self::Metadata {
        self
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> RawCapability {
        RawCapability {
            untyped: UntypedSlot::default()
                .with_base_phys_addr(meta.base.ppn())
                .with_free_phys_addr(meta.free.ppn())
                .with_size_log2(meta.size_log2),
        }
    }

    unsafe fn do_delete(_slot: SlotRefMut<'_, Self>) {
        todo!()
    }
}

impl<'a> From<SlotRef<'a, Untyped>> for Untyped {
    fn from(x: SlotRef<'a, Untyped>) -> Self {
        x.meta.clone()
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

struct DebugAdapterCoalesce<'a>(u64, &'a RawCapabilitySlot);

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
                        if l.1.cap == r.1.cap && l.1.dtnode == r.1.dtnode {
                            Ok(DebugAdapterCoalesce(l.0 + 1, r.1))
                        } else {
                            Err((l, r))
                        }
                    })
                    .map(|x| (x.1 as *const _, x)),
            )
            .finish()
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct DerivationTreeNode {
    first_child: DTNodeRef,
    parent: DTNodeRef,
    next_sibling: DTNodeRef,
    prev_sibling: DTNodeRef,
    _pad0: u64,
    _pad1: u64,
}

#[allow(clippy::missing_fields_in_debug)]
impl fmt::Debug for DerivationTreeNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DerivationTreeNode")
            .field("first_child", &self.first_child)
            .field("parent", &self.parent)
            .field("next_sibling", &self.next_sibling)
            .field("prev_sibling", &self.prev_sibling)
            .finish()
    }
}

const _ASSERT_DTNODE_SIZE_EQ_48: () = assert!(
    core::mem::size_of::<DerivationTreeNode>() == 48,
    "NullCapSlot size was not 48 bytes"
);

bitfield! {
    #[repr(C)]
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct DTNodeRef(u64);
    /// We only need 35 bits (39 - log2(64)) due to alignment.
    u64, slot_addr, set_slot_addr: 32, 0;
    /// We only need 27 bits (39 - log2(4096)) due to page alignment.
    u64, captbl_addr, set_captbl_addr: 59, 33;
}

impl fmt::Debug for DTNodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self == &DTNodeRef::null() {
            write!(f, "<null>")
        } else {
            f.debug_struct("DTNodeRef")
                .field("slot", &self.slot())
                .field("captbl", &self.captbl())
                .finish()
        }
    }
}

impl DTNodeRef {
    pub const NULL: Self = Self::null();

    pub const fn null() -> Self {
        Self(0)
    }

    pub fn new(slot: *const RawCapabilitySlot, captbl: *const CaptblHeader) -> Self {
        Self(0).with_slot(slot).with_captbl(captbl)
    }

    pub fn slot(&self) -> *const RawCapabilitySlot {
        canonicalize((self.slot_addr() << 6) as usize) as *const _
    }

    #[must_use]
    pub fn with_slot(mut self, slot: *const RawCapabilitySlot) -> Self {
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

impl<'a, C: Capability> SlotRefMut<'a, C> {
    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    pub fn add_child(&mut self, child: &mut SlotRefMut<'a, C>) {
        assert_eq!(
            child.slot.dtnode.parent,
            DTNodeRef::null(),
            "SlotRefMut::add_child: parent was not 0"
        );
        if self.slot.dtnode.first_child == DTNodeRef::null() {
            self.slot.dtnode.first_child =
                DTNodeRef::new(ptr::addr_of!(*child.slot), child.tbl_addr.into_ptr());
            child.slot.dtnode.parent =
                DTNodeRef::new(ptr::addr_of!(*self.slot), self.tbl_addr.into_ptr());
        } else {
            assert_eq!(
                child.slot.dtnode.next_sibling,
                DTNodeRef::null(),
                "SlotRefMut::add_child: next_sibling was not 0"
            );
            assert_eq!(
                child.slot.dtnode.prev_sibling,
                DTNodeRef::null(),
                "SlotRefMut::add_child: prev_sibling was not 0"
            );
            let next_sibling = self.slot.dtnode.first_child;
            self.slot.dtnode.first_child =
                DTNodeRef::new(ptr::addr_of!(*child.slot), child.tbl_addr.into_ptr());
            let self_addr = DTNodeRef::new(ptr::addr_of!(*self.slot), self.tbl_addr.into_ptr());
            child.slot.dtnode.first_child = self_addr;
            child.slot.dtnode.next_sibling = next_sibling;
            child.next_sibling_mut(|next_sibling| {
                next_sibling.slot.dtnode.prev_sibling = self_addr;
            });
        }
    }

    pub fn next_sibling_mut<T: 'a>(
        &mut self,
        f: impl for<'b> FnOnce(SlotRefMut<'b, C>) -> T,
    ) -> Option<T> {
        // SAFETY: This is always safe.
        let dtnode = unsafe {
            &*ptr::addr_of!(*self.slot)
                .add(1)
                .cast::<DerivationTreeNode>()
        };

        if dtnode.next_sibling == DTNodeRef::null() {
            return None;
        }

        let next_sibling_captbl_addr = Virtual::from_ptr(dtnode.next_sibling.captbl());

        let next_sibling_ptr = dtnode.next_sibling.slot().cast_mut();

        #[allow(clippy::if_not_else)]
        let val = if next_sibling_captbl_addr != self.tbl_addr {
            // SAFETY: We have made sure this pointer is
            // non-null. Additionally, this captbl is still valid as, if
            // our parent was destroyed, we would have been destroyed as
            // well.
            let next_sibling_captbl =
                unsafe { Captbl::from_raw_increment(next_sibling_captbl_addr) };
            let lock = next_sibling_captbl.write();

            let mut slot = lock.map(|_| {
                // SAFETY: We have the lock. We use `.map()` to ensure
                // this value only lasts as long as the lock.
                unsafe { &mut *(next_sibling_ptr) }
            });

            let meta = C::metadata_from_slot(&slot.cap);
            f(SlotRefMut {
                slot: &mut slot,
                meta,
                tbl_addr: next_sibling_captbl_addr,
                _phantom: PhantomData,
            })
        } else {
            // SAFETY: As the table addresses are the same, we already
            // have the lock by our invariants.
            let slot = unsafe { &mut *(next_sibling_ptr) };
            let meta = C::metadata_from_slot(&slot.cap);
            f(SlotRefMut {
                slot,
                meta,
                tbl_addr: next_sibling_captbl_addr,
                _phantom: PhantomData,
            })
        };

        Some(val)
    }

    pub fn parent_mut<T: 'a>(
        &mut self,
        f: impl for<'b> FnOnce(SlotRefMut<'b, C>) -> T,
    ) -> Option<T> {
        // SAFETY: This is always safe.
        let dtnode = unsafe {
            &*ptr::addr_of!(*self.slot)
                .add(1)
                .cast::<DerivationTreeNode>()
        };

        if dtnode.parent == DTNodeRef::null() {
            return None;
        }

        let parent_captbl_addr = Virtual::from_ptr(dtnode.parent.captbl());

        let parent_ptr = dtnode.parent.slot().cast_mut();

        #[allow(clippy::if_not_else)]
        let val = if parent_captbl_addr != self.tbl_addr {
            // SAFETY: We have made sure this pointer is
            // non-null. Additionally, this captbl is still valid as, if
            // our parent was destroyed, we would have been destroyed as
            // well.
            let parent_captbl = unsafe { Captbl::from_raw_increment(parent_captbl_addr) };
            let lock = parent_captbl.write();
            // SAFETY: We have the lock. We use `.map()` to ensure
            // this value only lasts as long as the lock.
            let mut slot = lock.map(|_| unsafe { &mut *(parent_ptr) });

            let meta = C::metadata_from_slot(&slot.cap);
            f(SlotRefMut {
                slot: &mut slot,
                meta,
                tbl_addr: parent_captbl_addr,
                _phantom: PhantomData,
            })
        } else {
            // SAFETY: As the table addresses are the same, we already
            // have the lock by our invariants.
            let slot = unsafe { &mut *(parent_ptr) };
            let meta = C::metadata_from_slot(&slot.cap);
            f(SlotRefMut {
                slot,
                meta,
                tbl_addr: parent_captbl_addr,
                _phantom: PhantomData,
            })
        };

        Some(val)
    }
}

#[repr(C, align(4096))]
pub struct ThreadControlBlock {
    trapframe: Trapframe,
    captbl: Option<Captbl>,
    root_pgtbl: Option<NonNull<()>>,
    state: ThreadState,
    context: Context,
    stack: TCBKernelStack,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ThreadState {
    #[num_enum(default)]
    Uninit = 0,
    Sleeping = 1,
    Runnable = 2,
    Running = 3,
}

#[repr(C, align(4096))]
pub struct TCBKernelStack {
    inner: [u8; THREAD_STACK_SIZE],
}

pub const THREAD_STACK_SIZE: usize = 4 * units::MIB;
