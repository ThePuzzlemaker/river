use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU16, AtomicU64};
use core::{fmt, mem, ptr};

use alloc::string::String;
use alloc::sync::Arc;
use atomic::{Atomic, Ordering};
use bitfield::bitfield;
use bytemuck::NoUninit;
use num_enum::{FromPrimitive, IntoPrimitive};
use rille::capability::paging::{
    BasePage, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel,
};
use rille::capability::{CapError, CapResult, CapRights, CapabilityType, UserRegisters};

use rille::addr::{DirectMapped, Identity, Physical, PhysicalMut, Ppn, VirtualConst};
use rille::units::{self};

use crate::hart_local::LOCAL_HART;
use crate::kalloc;
use crate::kalloc::phys::PMAlloc;
use crate::paging::{self, root_page_table, SharedPageTable};
use crate::proc::{Context, Scheduler};
use crate::sync::{SpinMutex, SpinRwLock, SpinRwLockWriteGuard};
use crate::trampoline::Trapframe;
use crate::trap::user_trap_ret;

use self::captbl::{Captbl, CaptblSlot, WeakCaptbl};
use self::derivation::DerivationTreeNode;
use self::slotref::{SlotRef, SlotRefMut};

pub mod captbl;
pub mod derivation;
pub mod slotref;

#[repr(C)]
pub union RawCapability {
    pub empty: EmptySlot,
    pub captbl: ManuallyDrop<CaptblSlot>,
    pub allocator: AllocatorSlot,
    pub pgtbl: ManuallyDrop<PgTblSlot>,
    pub page: PageSlot,
    pub thread: ManuallyDrop<ThreadSlot>,
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
    pub dtnode: DerivationTreeNode,
    pub cap: RawCapability,
    #[allow(clippy::missing_fields_in_debug)]
    pub cap_type: CapabilityType,
}

#[derive(Copy, Clone, Debug)]
#[repr(align(64))]
pub struct CaptblHeader {
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
        let slots = unsafe { &*self.inner.0 };

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
        let slots = unsafe { &*self.inner.0 };

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
    unsafe fn do_delete(
        meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    );
}

impl Capability for EmptySlot {
    type Metadata = ();

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Empty
    }

    fn into_meta(self) -> Self::Metadata {}
    fn metadata_from_slot(_: &RawCapabilitySlot) {}
    fn metadata_to_slot((): &Self::Metadata) -> (CapabilityType, RawCapability) {
        (CapabilityType::Empty, RawCapability { empty: EmptySlot })
    }
    unsafe fn do_delete((): Self::Metadata, _: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {}
}

impl<'a> CapToOwned for SlotRef<'a, EmptySlot> {
    type Target = EmptySlot;

    fn to_owned_cap(&self) -> Self::Target {
        EmptySlot
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, EmptySlot> {
    type Target = EmptySlot;

    fn to_owned_cap(&self) -> Self::Target {
        EmptySlot
    }
}

#[derive(Clone, Debug)]
pub enum AnyCap {
    Empty,
    Captbl(WeakCaptbl),
    Allocator,
    Page(Page),
    PgTbl(PgTblSlot),
    Thread(ThreadSlot),
    // TODO
}

impl AnyCap {
    pub fn cap_type(&self) -> CapabilityType {
        match self {
            AnyCap::Empty => CapabilityType::Empty,
            AnyCap::Captbl(_) => CapabilityType::Captbl,
            AnyCap::Allocator => CapabilityType::Allocator,
            AnyCap::Page(_) => CapabilityType::Page,
            AnyCap::PgTbl(_) => CapabilityType::PgTbl,
            AnyCap::Thread(_) => CapabilityType::Thread,
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
            CapabilityType::PgTbl => AnyCap::PgTbl(PgTbl::metadata_from_slot(slot)),
            CapabilityType::Page => AnyCap::Page(Page::metadata_from_slot(slot)),
            CapabilityType::Thread => AnyCap::Thread(Arc::<Thread>::metadata_from_slot(slot)),
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
            AnyCap::Allocator => AllocatorSlot::metadata_to_slot(&()),
            AnyCap::Page(page) => Page::metadata_to_slot(page),
            AnyCap::PgTbl(pgtbl) => PgTbl::metadata_to_slot(pgtbl),
            AnyCap::Thread(thread) => Arc::<Thread>::metadata_to_slot(thread),
        }
    }

    unsafe fn do_delete(
        meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    ) {
        // SAFETY: Our invariants ensure this isn't called twice, and
        // we downcast correctly.
        unsafe {
            match meta {
                AnyCap::Empty => {}
                AnyCap::Captbl(tbl) => Captbl::do_delete(tbl, slot),
                AnyCap::Allocator => AllocatorSlot::do_delete((), slot),
                AnyCap::Page(pg) => Page::do_delete(pg, slot),
                AnyCap::PgTbl(pgtbl) => PgTbl::do_delete(pgtbl, slot),
                AnyCap::Thread(thread) => Arc::<Thread>::do_delete(thread, slot),
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
            AnyCap::Page(page) => AnyCap::Page(page.clone()),
            AnyCap::PgTbl(pgtbl) => AnyCap::PgTbl(pgtbl.clone()),
            AnyCap::Thread(thread) => AnyCap::Thread(thread.clone()),
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
            AnyCap::Page(page) => AnyCap::Page(page.clone()),
            AnyCap::PgTbl(pgtbl) => AnyCap::PgTbl(pgtbl.clone()),
            AnyCap::Thread(thread) => AnyCap::Thread(thread.clone()),
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
        slot.slot.dtnode = DerivationTreeNode::default();
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

bitfield! {
    #[derive(Copy, Clone, PartialEq, Eq)]
    struct PpnSizeLog2(u64);
    impl Debug;
    pub u64, from into Ppn, get_ppn, set_ppn: 44, 0;
    pub u8, get_size_log2, set_size_log2: 52, 45;
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageSlot {
    ppn_size_log2: PpnSizeLog2,
    pub rights: CapRights,
}

#[derive(Clone, Debug)]
pub struct Page {
    phys: PhysicalMut<u8, DirectMapped>,
    size_log2: u8,
    rights: CapRights,
}

impl Page {
    /// Create a page capability from a physical memory allocation.
    ///
    /// # Safety
    ///
    /// - `phys` must have been allocated by [`PMAlloc`].
    /// - `phys` must be valid for `1 << size_log2` bytes.
    /// - `phys` must be aligned to `1 << size_log2` bytes.
    pub unsafe fn new(phys: PhysicalMut<u8, DirectMapped>, size_log2: u8) -> Self {
        Self {
            phys,
            size_log2,
            rights: CapRights::all(),
        }
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
            phys: Physical::from_components(slot.ppn_size_log2.get_ppn(), None),
            size_log2: slot.ppn_size_log2.get_size_log2(),
            rights: slot.rights,
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
                    ppn_size_log2: {
                        let mut x = PpnSizeLog2(0);
                        x.set_ppn(meta.phys.ppn());
                        x.set_size_log2(meta.size_log2);
                        x
                    },
                    rights: meta.rights,
                },
            },
        )
    }

    unsafe fn do_delete(meta: Self::Metadata, _: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        let mut pma = PMAlloc::get();
        // SAFETY: By our invariants, this memory must be initialized and have been allocated by PMAlloc.
        unsafe { pma.deallocate(meta.phys, kalloc::phys::what_order(1 << meta.size_log2)) };
    }
}

#[derive(Clone)]
pub struct PgTblSlot {
    pub table: SharedPageTable,
}

#[derive(Clone)]
pub struct PgTbl {
    table: SharedPageTable,
}

impl fmt::Debug for PgTblSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgTblSlot")
            .field("<ptr:phys>", &self.table.as_physical_const())
            // SAFETY: table is modified atomically
            .field("tbl", &self.table)
            .finish()
    }
}

impl fmt::Debug for PgTbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgTbl")
            .field("<ptr:phys>", &self.table.as_physical_const())
            // SAFETY: table is modified atomically
            .field("tbl", &self.table)
            .finish()
    }
}

impl PgTbl {
    pub fn new(table: SharedPageTable) -> Self {
        Self { table }
    }
}

impl Capability for PgTbl {
    type Metadata = PgTblSlot;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::PgTbl
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants.
        ManuallyDrop::into_inner(unsafe { &slot.cap.pgtbl }.clone())
    }

    fn into_meta(self) -> Self::Metadata {
        PgTblSlot { table: self.table }
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::PgTbl,
            RawCapability {
                pgtbl: ManuallyDrop::new(meta.clone()),
            },
        )
    }

    unsafe fn do_delete(
        _meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    ) {
        let raw_slot = mem::replace(&mut slot.cap, RawCapability { empty: EmptySlot });
        // SAFETY: By invariants
        drop(ManuallyDrop::into_inner(unsafe { raw_slot.pgtbl }));
    }
}

impl<'a> SlotRef<'a, PgTbl> {
    pub fn table(&self) -> &SharedPageTable {
        &self.meta.table
    }
}

impl<'a> SlotRefMut<'a, PgTbl> {
    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn map_page(
        &mut self,
        page: &mut SlotRefMut<'_, Page>,
        addr: VirtualConst<u8, Identity>,
        flags: PageTableFlags,
    ) -> CapResult<()> {
        use paging::PageTableFlags as KernelFlags;

        if addr.into_usize() >= 0x40_0000_0000 {
            return Err(CapError::InvalidOperation);
        }

        if addr.into_usize() % (1 << (page.meta.size_log2 as usize)) != 0 {
            return Err(CapError::InvalidSize);
        }

        let flags = flags & page.meta.rights.into_pgtbl_mask();

        let flags =
            KernelFlags::from_bits_truncate(flags.bits()) | KernelFlags::VAD | KernelFlags::USER;

        {
            let page_size = match page.meta.size_log2 as usize {
                BasePage::PAGE_SIZE_LOG2 => PageSize::Base,
                MegaPage::PAGE_SIZE_LOG2 => PageSize::Mega,
                GigaPage::PAGE_SIZE_LOG2 => PageSize::Giga,
                _ => unreachable!(),
            };
            self.meta.table.map(
                page.meta.phys.into_identity().into_const(),
                addr,
                flags,
                page_size,
            );
        }
        // TODO: sfence
        self.add_child(page);

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

    fn metadata_from_slot(_: &RawCapabilitySlot) {}

    fn into_meta(self) {}

    fn metadata_to_slot((): &()) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Allocator,
            RawCapability {
                allocator: AllocatorSlot,
            },
        )
    }

    unsafe fn do_delete((): Self::Metadata, _: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>) {
        //        todo!()
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
            .field("cap", &RawCapDebugAdapter(self))
            .field("dtnode", &self.dtnode)
            .finish_non_exhaustive()
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
            // SAFETY: Type check.
            CapabilityType::PgTbl => unsafe { &self.0.cap.pgtbl }.fmt(f),
            // SAFETY: Type check.
            CapabilityType::Page => unsafe { &self.0.cap.page }.fmt(f),
            CapabilityType::Thread => {
                // SAFETY: Type check.
                let x = unsafe { &self.0.cap.thread.inner };
                write!(f, "Thread(<opaque:{:#p}>)", Arc::as_ptr(x),)
            }
            CapabilityType::Unknown => f.debug_struct("InvalidCap").finish(),
        }
    }
}

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

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct ThreadSlot {
    inner: Arc<Thread>,
}

impl Capability for Arc<Thread> {
    type Metadata = ThreadSlot;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Thread
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants
        ManuallyDrop::into_inner(unsafe { slot.cap.thread.clone() })
    }

    fn into_meta(self) -> Self::Metadata {
        ThreadSlot { inner: self }
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Thread,
            RawCapability {
                thread: ManuallyDrop::new(meta.clone()),
            },
        )
    }

    unsafe fn do_delete(
        meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    ) {
        let raw_slot = mem::replace(&mut slot.cap, RawCapability { empty: EmptySlot });

        // SAFETY: By invariants
        drop(ManuallyDrop::into_inner(unsafe { raw_slot.thread }));

        // If we're an orphan dtnode, extract our captbl and drop it
        // This breaks any cycles that prevent us from being fully
        // dropped.
        if slot.dtnode.first_child.is_none()
            && slot.dtnode.next_sibling.is_none()
            && slot.dtnode.prev_sibling.is_none()
            && slot.dtnode.parent.is_none()
        {
            let mut private = meta.inner.private.write();
            drop(private.captbl.take());
            drop(private);
            drop(meta);
        }
    }
}

impl<'a> SlotRef<'a, Arc<Thread>> {
    pub fn suspend(&self) {
        Scheduler::dequeue(
            self.meta.inner.hartid.load(Ordering::Relaxed),
            self.meta.inner.tid,
        );
        // Release ordering ensures that the dequeue operation
        // strictly happens-before this release operation, which then
        // happens-before any accompanying Acquire load for suspend
        // tests.
        self.meta
            .inner
            .state
            .store(ThreadState::Suspended, Ordering::Release);
    }

    /// Configure the captbl and page table of a thread.
    ///
    /// # Errors
    ///
    /// See [`Captr::<Thread>::configure`][1].
    ///
    /// [1]: rille::capability::Captr::<rille::capability::Thread>::configure
    pub fn configure(&self, captbl: Captbl, pgtbl: SharedPageTable) -> CapResult<()> {
        if self.meta.inner.state.load(Ordering::Acquire) != ThreadState::Suspended
            && self.meta.inner.state.load(Ordering::Acquire) != ThreadState::Uninit
        {
            return Err(CapError::InvalidOperation);
        }

        let mut private = self.meta.inner.private.write();
        private.captbl = Some(captbl);
        let old_pgtbl = private.root_pgtbl.replace(pgtbl);
        if let Some(_pgtbl) = old_pgtbl {
            // TODO: pgtbl.unmap(VirtualConst::from(private.trapframe_addr))
        }

        drop(private);

        self.meta.inner.setup_page_table();
        self.meta
            .inner
            .state
            .store(ThreadState::Suspended, Ordering::Release);

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
        let cont = {
            let private = self.meta.inner.private.read();
            private.root_pgtbl.is_some()
        };

        if !cont {
            return Err(CapError::InvalidOperation);
        }

        self.meta
            .inner
            .state
            .store(ThreadState::Runnable, Ordering::Relaxed);
        Scheduler::enqueue(self.meta.inner.clone());

        Ok(())
    }

    /// Write the trapframe a thread.
    ///
    /// # Errors
    ///
    /// See [`Captr::<Thread>::write_regsters`][1].
    ///
    /// [1]: rille::capability::Captr::<rille::capability::Thread>::write_registers
    ///
    pub fn write_registers(
        &self,
        registers: VirtualConst<UserRegisters, Identity>,
    ) -> CapResult<()> {
        use paging::PageTableFlags as KernelFlags;

        if self.meta.inner.state.load(Ordering::Acquire) != ThreadState::Suspended {
            return Err(CapError::InvalidOperation);
        }

        let private = self.meta.inner.private.read();
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

        let mut trapframe = self.meta.inner.trapframe.lock();
        let trapframe = &mut *trapframe;
        let old_trapframe = trapframe.as_ref();
        *trapframe.as_mut() = Trapframe {
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

#[repr(C)]
pub struct Thread {
    pub trapframe: SpinMutex<OwnedTrapframe>,
    pub stack: ThreadKernelStack,
    pub private: SpinRwLock<ThreadProtected>,
    pub context: SpinMutex<Context>,
    pub state: Atomic<ThreadState>,
    pub tid: usize,
    pub hartid: AtomicU64,
}

pub struct OwnedTrapframe {
    inner: PhysicalMut<Trapframe, DirectMapped>,
}

impl OwnedTrapframe {
    pub fn new() -> Self {
        let mut pma = PMAlloc::get();
        Self {
            inner: pma.allocate(0).unwrap().cast(),
        }
    }

    pub fn as_phys(&self) -> PhysicalMut<Trapframe, DirectMapped> {
        self.inner
    }

    // TODO: make these Deref/Mut?
    pub fn as_ref(&self) -> &Trapframe {
        // SAFETY: By invariants.
        unsafe { &*self.inner.into_virt().into_ptr_mut() }
    }

    pub fn as_mut(&mut self) -> &mut Trapframe {
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
        let mut this = Arc::<Thread>::new_uninit();
        let this_mut = Arc::get_mut(&mut this).unwrap();

        let tid = next_tid();

        // SAFETY: These initialization addresses are valid.
        unsafe {
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).trapframe)
                .write(SpinMutex::new(OwnedTrapframe::new()));
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).stack).write(ThreadKernelStack::new());
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).private).write(SpinRwLock::new(
                ThreadProtected {
                    captbl,
                    root_pgtbl: pgtbl,
                    asid: 1, // TODO
                    name,
                    trapframe_addr: 0,
                },
            ));
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).state)
                .write(Atomic::new(ThreadState::Uninit));
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).tid).write(tid as usize);
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).hartid).write(AtomicU64::new(u64::MAX));
        }

        // SAFETY: We have just initialized this.
        // - context is zeroable.
        let this = unsafe { this.assume_init() };
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
        use paging::PageTableFlags as KernelFlags;

        let mut private = self.private.write();
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
    pub unsafe fn yield_to_scheduler_final(mut self: Arc<Thread>) {
        LOCAL_HART.with(|hart| {
            let intena = hart.intena.get();

            let this = Arc::get_mut(&mut self).unwrap();
            let ctx = mem::take(&mut this.context);
            drop(self);
            {
                // SAFETY: The scheduler context is always valid, as
                // guaranteed by the scheduler.
                unsafe { Context::switch(&hart.context, &ctx) }
            }

            hart.intena.set(intena);
        });
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
    pub unsafe fn yield_to_scheduler() {
        LOCAL_HART.with(|hart| {
            let intena = hart.intena.get();

            let ctx_ptr = {
                let proc = hart.thread.borrow();
                let proc = proc.as_ref().unwrap();
                core::ptr::addr_of!(proc.context)
            };

            {
                // SAFETY: As the scheduler is currently running this
                // process, it retains an `Arc` to it (We cannot
                // safely dequeue a process without it being suspended
                // first). Thus, this pointer remains valid.
                let context = unsafe { &*ctx_ptr };
                // SAFETY: The scheduler context is always valid, as
                // guaranteed by the scheduler.
                unsafe { Context::switch(&hart.context, context) }
            }

            hart.intena.set(intena);
        });
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("private", &self.private)
            .field("context", &self.context)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

pub struct ThreadProtected {
    pub captbl: Option<Captbl>,
    pub root_pgtbl: Option<SharedPageTable>,
    pub asid: u16,
    pub name: String,
    pub trapframe_addr: usize,
}

impl fmt::Debug for ThreadProtected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadProtected")
            .field("root_pgtbl", &self.root_pgtbl)
            .field("asid", &self.asid)
            .field("name", &self.name)
            .field("trapframe_addr", &self.trapframe_addr)
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
            )
        }
    }
}

// TODO: guard page
#[cfg(debug_assertions)]
pub const THREAD_STACK_SIZE: usize = 4 * units::MIB;
#[cfg(not(debug_assertions))]
pub const THREAD_STACK_SIZE: usize = 512 * units::KIB;

pub const TRAPFRAME_ADDR: usize = usize::MAX - 3 * 4 * units::KIB + 1;
