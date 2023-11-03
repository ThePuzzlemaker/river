use core::{fmt, marker::PhantomData};

use crate::sync::{SpinRwLockReadGuard, SpinRwLockWriteGuard};

use super::{AnyCap, Capability, RawCapabilitySlot};

pub struct SlotRef<'a, C: Capability> {
    pub(super) slot: SpinRwLockReadGuard<'a, RawCapabilitySlot>,
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
            _phantom: PhantomData,
        }
    }

    pub fn as_ptr(&self) -> *const RawCapabilitySlot {
        &*self.slot
    }
}

pub struct SlotRefMut<'a, C: Capability> {
    pub(super) slot: SpinRwLockWriteGuard<'a, RawCapabilitySlot>,
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
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn upcast_mut(self) -> SlotRefMut<'a, AnyCap> {
        let meta = AnyCap::metadata_from_slot(&self.slot.cap);
        SlotRefMut {
            slot: self.slot,
            meta,
            _phantom: PhantomData,
        }
    }

    pub fn as_ptr(&mut self) -> *mut RawCapabilitySlot {
        &mut *self.slot
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
