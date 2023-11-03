use core::{
    fmt,
    mem::{ManuallyDrop, MaybeUninit},
    ops::Deref,
    ptr::NonNull,
    sync::atomic::AtomicU64,
};

use bitfield::bitfield;
use rille::{
    addr::{DirectMapped, VirtualConst, VirtualMut},
    capability::CapabilityType,
};

use crate::{
    capability::EmptySlot,
    sync::{arc_like::ArcLike, SpinRwLock},
};

use super::{
    derivation::DerivationTreeNode, CapToOwned, Capability, CapabilitySlot, CaptblHeader,
    RawCapability, RawCapabilitySlot, SlotRef, SlotRefMut,
};

pub struct Captbl {
    pub(super) hdr: NonNull<CaptblHeader>,
}

impl fmt::Debug for Captbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Captbl")
            .field("<ptr>", &self.hdr.as_ptr())
            .field("hdr", &**self)
            .field("table", &self.slots())
            .finish()
    }
}

impl PartialEq for Captbl {
    fn eq(&self, other: &Self) -> bool {
        self.hdr == other.hdr
    }
}

impl Eq for Captbl {}

impl Captbl {
    /// Create a new capability table, given a virtual address and the
    /// number of slots, as a base 2 logarithm.
    ///
    /// # Safety
    ///
    /// - `base` must be a valid pointer to mapped memory.
    ///
    /// - `base` must be valid for `1 << (n_slots_log2 + 5)` ==
    /// `2.pow(n_slots_log2) * 32` bytes.
    ///
    /// - `n_slots_log2` must not be 0.
    pub unsafe fn new(
        base: VirtualMut<u8, DirectMapped>,
        n_slots_log2: u8,
        untyped: Option<NonNull<CapabilitySlot>>,
    ) -> Self {
        let header = CaptblHeader {
            refcount: AtomicU64::new(1),
            n_slots_log2,
            untyped,
        };

        let captbl: *mut MaybeUninit<CaptblHeader> = base.cast().into_ptr_mut();
        // SAFETY: This memory is valid, and it is valid to read it
        // even if it's uninitialized.
        let captbl = unsafe { &mut *captbl };
        captbl.write(header);

        // SAFETY: This pointer is non-null by our invariants.
        let hdr = unsafe { NonNull::new_unchecked(captbl.as_mut_ptr()) };

        let mut tbl = Self { hdr };

        // SAFETY: We own this memory right now.
        for slot in unsafe { tbl.slots_uninit() } {
            slot.write(CapabilitySlot {
                lock: SpinRwLock::new(RawCapabilitySlot {
                    cap: RawCapability {
                        empty: EmptySlot::default(),
                    },
                    dtnode: DerivationTreeNode::default(),
                }),
            });
        }

        tbl
    }

    /// TODO
    ///
    /// # Safety
    ///
    /// TODO
    pub unsafe fn from_raw_increment(ptr: VirtualConst<CaptblHeader, DirectMapped>) -> Self {
        let x = Self {
            // SAFETY: By invariants.
            hdr: unsafe { NonNull::new_unchecked(ptr.into_ptr_mut()) },
        };

        // SAFETY: By invariants.
        unsafe { x.increase_refcount() };

        x
    }

    #[inline]
    fn inner(&self) -> &CaptblHeader {
        // SAFETY: By our invariants, this is safe.
        unsafe { self.hdr.as_ref() }
    }
}

// SAFETY: We properly synchronize Captbls.
unsafe impl Send for Captbl {}
// SAFETY: See above.
unsafe impl Sync for Captbl {}

// SAFETY: We follow all invariants of `ArcLike` and do not override
// default functions.
unsafe impl ArcLike for Captbl {
    type Target = CaptblHeader;

    fn refcount(arc: &Self) -> &AtomicU64 {
        &arc.inner().refcount
    }

    fn data(arc: &Self) -> NonNull<CaptblHeader> {
        arc.hdr
    }

    unsafe fn deallocate(_arc: &mut Self) {
        // todo: handle returning this memory to the untyped
    }

    unsafe fn clone_ref(arc: &Self) -> Self {
        Self { hdr: arc.hdr }
    }
}

crate::impl_arclike_traits!(Captbl);

impl Capability for Captbl {
    type Metadata = ManuallyDrop<Captbl>;

    fn is_slot_valid_type(slot: &RawCapability) -> bool {
        slot.cap_type() == CapabilityType::Captbl
    }

    fn metadata_from_slot(slot: &RawCapability) -> Self::Metadata {
        ManuallyDrop::new(Captbl {
            // SAFETY: By the invariants of the Captbl slot, this is
            // safe. Also, we don't need to increase the refcount as
            // to put this in the slot, we had to refcount.
            hdr: unsafe { NonNull::new_unchecked(slot.captbl.virt_addr().into_ptr_mut().cast()) },
        })
    }

    fn into_meta(self) -> Self::Metadata {
        ManuallyDrop::new(self)
    }

    fn metadata_to_slot(meta: &Self::Metadata) -> RawCapability {
        RawCapability {
            captbl: ManuallyDrop::new(CaptblSlot::new(ManuallyDrop::into_inner(meta.clone()))),
        }
    }

    unsafe fn do_delete(slot: SlotRefMut<'_, Captbl>) {
        drop(ManuallyDrop::into_inner(slot.meta));
    }
}

impl<'a> CapToOwned for SlotRef<'a, Captbl> {
    type Target = Captbl;

    fn to_owned_cap(&self) -> Self::Target {
        (*self).clone()
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, Captbl> {
    type Target = Captbl;

    fn to_owned_cap(&self) -> Self::Target {
        (*self).clone()
    }
}

impl<'a> Deref for SlotRef<'a, Captbl> {
    type Target = Captbl;

    fn deref(&self) -> &Captbl {
        &self.meta
    }
}

impl<'a> Deref for SlotRefMut<'a, Captbl> {
    type Target = Captbl;

    fn deref(&self) -> &Captbl {
        &self.meta
    }
}

bitfield! {
    #[repr(C)]
    pub struct CaptblSlot(u128);
    impl Debug;
    u8, from into CapabilityType, cap_type, set_cap_type: 4, 0;
    u64, from into VirtualConst<CaptblHeader, DirectMapped>, virt_addr, set_virt_addr: 43, 5;
}

impl Clone for CaptblSlot {
    fn clone(&self) -> Self {
        Self(0)
            .with_cap_type(CapabilityType::Captbl)
            .with_virt_addr(self.virt_addr())
    }
}

impl CaptblSlot {
    /// Create a new `CaptblSlot` from a [`Captbl`].
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(captbl: Captbl) -> Self {
        let virt_addr = VirtualConst::from_ptr(captbl.hdr.as_ptr());
        Self(0)
            .with_cap_type(CapabilityType::Captbl)
            .with_virt_addr(virt_addr)
    }

    #[inline]
    fn with_cap_type(mut self, cap_type: CapabilityType) -> Self {
        self.set_cap_type(cap_type);
        self
    }

    #[must_use]
    #[inline]
    fn with_virt_addr(mut self, virt_addr: VirtualConst<CaptblHeader, DirectMapped>) -> Self {
        self.set_virt_addr(virt_addr);
        self
    }
}

impl PartialEq for CaptblSlot {
    fn eq(&self, other: &Self) -> bool {
        self.virt_addr() == other.virt_addr()
    }
}
