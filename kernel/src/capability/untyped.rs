use bitfield::bitfield;
use rille::{
    addr::{DirectMapped, Physical, PhysicalMut, Ppn},
    capability::CapabilityType,
};

use super::{
    slotref::{SlotRef, SlotRefMut},
    CapToOwned, Capability, RawCapability,
};

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

impl<'a> CapToOwned for SlotRef<'a, Untyped> {
    type Target = Untyped;

    fn to_owned_cap(&self) -> Self::Target {
        self.meta.clone()
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, Untyped> {
    type Target = Untyped;

    fn to_owned_cap(&self) -> Self::Target {
        self.meta.clone()
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

impl<'a> SlotRef<'a, Untyped> {
    pub fn free_addr(&self) -> PhysicalMut<u8, DirectMapped> {
        self.meta.free
    }

    pub fn base_addr(&self) -> PhysicalMut<u8, DirectMapped> {
        self.meta.base
    }

    pub fn size_log2(&self) -> u8 {
        self.meta.size_log2
    }
}

impl<'a> SlotRefMut<'a, Untyped> {
    pub fn free_addr(&self) -> PhysicalMut<u8, DirectMapped> {
        self.meta.free
    }

    pub fn base_addr(&self) -> PhysicalMut<u8, DirectMapped> {
        self.meta.base
    }

    pub fn size_log2(&self) -> u8 {
        self.meta.size_log2
    }

    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    pub fn set_free_addr(&mut self, addr: PhysicalMut<u8, DirectMapped>) {
        assert!(addr <= self.base_addr().add(1 << self.size_log2()));
        assert!(addr > self.base_addr());
        self.meta.free = addr;
        self.slot.cap = Untyped::metadata_to_slot(&self.meta);
    }
}
