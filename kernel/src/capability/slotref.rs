use core::{fmt, marker::PhantomData};

use rille::addr::{DirectMapped, VirtualConst};

use super::{AnyCap, Capability, CaptblHeader, RawCapabilitySlot};

pub struct SlotRef<'a, C: Capability> {
    pub(super) slot: &'a RawCapabilitySlot,
    pub(super) tbl_addr: VirtualConst<CaptblHeader, DirectMapped>,
    pub(super) meta: C::Metadata,
    pub(super) _phantom: PhantomData<C>,
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

    pub fn as_ptr(&self) -> *const RawCapabilitySlot {
        self.slot
    }

    pub fn table_ptr(&self) -> *const CaptblHeader {
        self.tbl_addr.into_ptr()
    }
}

pub struct SlotRefMut<'a, C: Capability> {
    pub(super) slot: &'a mut RawCapabilitySlot,
    pub(super) tbl_addr: VirtualConst<CaptblHeader, DirectMapped>,
    pub(super) meta: C::Metadata,
    pub(super) _phantom: PhantomData<C>,
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

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn downgrade(&'_ mut self) -> SlotRef<'_, C> {
        SlotRef {
            slot: self.slot,
            meta: C::metadata_from_slot(&self.slot.cap),
            tbl_addr: self.tbl_addr,
            _phantom: PhantomData,
        }
    }

    pub fn upcast_mut(self) -> SlotRefMut<'a, AnyCap> {
        let meta = AnyCap::metadata_from_slot(&self.slot.cap);
        SlotRefMut {
            slot: self.slot,
            meta,
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

    pub fn as_ptr(&mut self) -> *mut RawCapabilitySlot {
        self.slot
    }

    pub fn table_ptr(&mut self) -> *const CaptblHeader {
        self.tbl_addr.into_ptr()
    }
}

// impls

impl<'a, C: Capability> fmt::Debug for SlotRef<'a, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotRef")
            .field("slot", &self.slot)
            .field("meta", &self.meta)
            .field("_phantom", &self._phantom)
            .finish()
    }
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
