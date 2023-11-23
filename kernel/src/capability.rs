use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::sync::atomic::AtomicU16;
use core::{fmt, mem, ptr};

use alloc::string::String;
use alloc::sync::Arc;
use atomic::{Atomic, Ordering};
use bytemuck::NoUninit;
use num_enum::{FromPrimitive, IntoPrimitive};
use rille::capability::paging::{
    BasePage, GigaPage, MegaPage, PageSize, PageTableFlags, PagingLevel,
};
use rille::capability::{CapError, CapResult, CapabilityType};

use rille::addr::{DirectMapped, Identity, Kernel, Physical, PhysicalMut, Ppn, VirtualConst};
use rille::units::{self, StorageUnits};

use crate::hart_local::LOCAL_HART;
use crate::kalloc::phys::PMAlloc;
use crate::paging::{self, root_page_table, SharedPageTable};
use crate::proc::{Context, Scheduler};
use crate::sync::{SpinMutex, SpinRwLock, SpinRwLockWriteGuard};
use crate::trampoline::Trapframe;
use crate::trap::user_trap_ret;
use crate::{kalloc, symbol};

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
    pub cap: RawCapability,
    #[allow(clippy::missing_fields_in_debug)]
    pub cap_type: CapabilityType,
    pub dtnode: DerivationTreeNode,
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
    PgTbl(ManuallyDrop<PgTblSlot>),
    Thread(ManuallyDrop<ThreadSlot>),
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
    /// Create a page capability from a physical memory allocation.
    ///
    /// # Safety
    ///
    /// - `phys` must have been allocated by [`PMAlloc`].
    /// - `phys` must be valid for `1 << size_log2` bytes.
    /// - `phys` must be aligned to `1 << size_log2` bytes.
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
    type Metadata = ManuallyDrop<PgTblSlot>;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::PgTbl
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants.
        unsafe { &slot.cap.pgtbl }.clone()
    }

    fn into_meta(self) -> Self::Metadata {
        ManuallyDrop::new(PgTblSlot { table: self.table })
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::PgTbl,
            RawCapability {
                pgtbl: meta.clone(),
            },
        )
    }

    unsafe fn do_delete(
        meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    ) {
        drop(ManuallyDrop::into_inner(meta));
        let raw_slot = mem::replace(&mut slot.cap, RawCapability { empty: EmptySlot });
        // SAFETY: By invariants
        drop(ManuallyDrop::into_inner(unsafe { raw_slot.pgtbl }));
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
            // SAFETY: Type check.
            CapabilityType::Thread => unsafe { &self.0.cap.thread }.fmt(f),
            CapabilityType::Unknown => f.debug_struct("InvalidCap").finish(),
        }
    }
}

static NEXT_ASID: AtomicU16 = AtomicU16::new(1);

fn next_asid() -> u16 {
    let v = NEXT_ASID.fetch_add(1, Ordering::Relaxed);
    assert!(
        core::intrinsics::likely(v != u16::MAX),
        "oops! ran out of ASIDs"
    );
    v
}

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
    type Metadata = ManuallyDrop<ThreadSlot>;

    fn is_valid_type(cap_type: CapabilityType) -> bool {
        cap_type == CapabilityType::Thread
    }

    fn metadata_from_slot(slot: &RawCapabilitySlot) -> Self::Metadata {
        // SAFETY: By invariants
        unsafe { slot.cap.thread.clone() }
    }

    fn into_meta(self) -> Self::Metadata {
        ManuallyDrop::new(ThreadSlot { inner: self })
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> (CapabilityType, RawCapability) {
        (
            CapabilityType::Thread,
            RawCapability {
                thread: meta.clone(),
            },
        )
    }

    unsafe fn do_delete(
        meta: Self::Metadata,
        slot: &mut SpinRwLockWriteGuard<'_, RawCapabilitySlot>,
    ) {
        // If we're an orphan dtnode, extract our captbl and drop it
        // first. This breaks any cycles that prevent us from being
        // fully dropped.
        if slot.dtnode.first_child.is_none()
            && slot.dtnode.next_sibling.is_none()
            && slot.dtnode.prev_sibling.is_none()
            && slot.dtnode.parent.is_none()
        {
            let mut private = meta.inner.private.write();
            drop(private.captbl.take());
        }

        drop(ManuallyDrop::into_inner(meta));
        let raw_slot = mem::replace(&mut slot.cap, RawCapability { empty: EmptySlot });
        // SAFETY: By invariants
        drop(ManuallyDrop::into_inner(unsafe { raw_slot.thread }));
    }
}

impl<'a> SlotRef<'a, Arc<Thread>> {
    pub fn suspend(&self, private: &ThreadProtected) {
        self.meta
            .inner
            .state
            .store(ThreadState::Suspended, Ordering::Relaxed);
        // TODO: notify the hart running the thread to stop its execution, or not?
        Scheduler::dequeue(private.hartid, self.meta.inner.tid);
    }
}

#[repr(C, align(4096))]
pub struct Thread {
    pub trapframe: SpinMutex<Trapframe>,
    pub stack: ThreadKernelStack,
    pub private: SpinRwLock<ThreadProtected>,
    pub context: SpinMutex<Context>,
    pub state: Atomic<ThreadState>,
    pub tid: usize,
}

impl Thread {
    pub fn new(name: String, captbl: Option<Captbl>, pgtbl: SharedPageTable) -> Arc<Thread> {
        use paging::PageTableFlags as KernelFlags;
        let mut this = Arc::<Thread>::new_uninit();
        let this_mut = Arc::get_mut(&mut this).unwrap();

        let tid = next_tid();

        // SAFETY: These initialization addresses are valid.
        unsafe {
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).private).write(SpinRwLock::new(
                ThreadProtected {
                    captbl,
                    root_pgtbl: pgtbl,
                    asid: 1, // TODO
                    name,
                    hartid: u64::MAX,
                },
            ));
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).state)
                .write(Atomic::new(ThreadState::Uninit));
            ptr::addr_of_mut!((*((*this_mut).as_mut_ptr())).tid).write(tid as usize);
        }

        // SAFETY: We have just initialized this.
        // - trapframe, stack, and context are zeroable.
        let this = unsafe { this.assume_init() };

        {
            let mut private = this.private.write();
            let trapframe_virt =
                VirtualConst::<_, Identity>::from_ptr(ptr::addr_of!(this.trapframe));
            let (trapframe_phys, _) = { root_page_table().lock().walk(trapframe_virt).unwrap() };

            // We only need to worry about the first page, as that's
            // where the trapframe will be. Nothing else will be
            // accessed from the user page tables.
            private.root_pgtbl.map(
                trapframe_phys.cast().into_identity(),
                VirtualConst::from_usize(TRAPFRAME_ADDR),
                KernelFlags::VAD | KernelFlags::RW,
                PageSize::Base,
            );

            let trampoline_virt =
                VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());

            private.root_pgtbl.map(
                trampoline_virt.into_phys().into_identity(),
                VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
                KernelFlags::VAD | KernelFlags::RX,
                PageSize::Base,
            );
        }
        {
            this.context.lock().ra = user_trap_ret as usize as u64;
            this.context.lock().sp = ptr::addr_of!(this.stack) as u64 + THREAD_STACK_SIZE as u64;
        }

        this
    }

    /// Yield this process's time to the scheduler.
    ///
    /// # Safety
    ///
    /// The provided thread must be running on the current hart.
    ///
    /// # Guarantees and Deadlocks
    ///
    /// Locks **must not** be held when calling this function, else
    /// there is a large chance for deadlock.
    ///
    /// The scheduler will constrain the user thread's stream of
    /// execution to the current hart. To allow a thread to be stolen
    /// by another hart, the waitlist must be used.
    pub unsafe fn yield_to_scheduler(&self) {
        LOCAL_HART.with(|hart| {
            let intena = hart.intena.get();

            // SAFETY: The scheduler context is always valid, as
            // guaranteed by the scheduler.
            unsafe { Context::switch(&hart.context, &self.context) }

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

#[derive(Debug)]
pub struct ThreadProtected {
    pub captbl: Option<Captbl>,
    pub root_pgtbl: SharedPageTable,
    pub asid: u16,
    pub name: String,
    pub hartid: u64,
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

#[repr(C, align(4096))]
pub struct ThreadKernelStack {
    inner: [u8; THREAD_STACK_SIZE],
}

// TODO: guard page
#[cfg(debug_assertions)]
pub const THREAD_STACK_SIZE: usize = 4 * units::MIB;
#[cfg(not(debug_assertions))]
pub const THREAD_STACK_SIZE: usize = 512 * units::KIB;

pub const TRAPFRAME_ADDR: usize = usize::MAX - 3 * 4 * units::KIB + 1;
