use core::{
    fmt,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use rille::capability::{AnyCap, CapabilityType};

use crate::sync::{SpinRwLockReadGuard, SpinRwLockWriteGuard};

use super::{Capability, CapabilitySlotInner};

pub struct SlotRef<'a, C: Capability> {
    pub(super) slot: SpinRwLockReadGuard<'a, CapabilitySlotInner>,
    pub(super) _phantom: PhantomData<C>,
}

impl<'a, C: Capability> Deref for SlotRef<'a, C> {
    type Target = CapabilitySlotInner;

    fn deref(&'_ self) -> &'_ Self::Target {
        &self.slot
    }
}

impl<'a> SlotRef<'a, AnyCap> {
    pub fn cap_type(&self) -> CapabilityType {
        self.cap.cap_type()
    }

    pub fn downcast<C: Capability>(self) -> Option<SlotRef<'a, C>> {
        if C::is_valid_type(self.cap.cap_type()) {
            Some(SlotRef {
                slot: self.slot,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a, C: Capability> SlotRef<'a, C> {
    pub fn upcast(self) -> SlotRef<'a, AnyCap> {
        SlotRef {
            slot: self.slot,
            _phantom: PhantomData,
        }
    }
}

pub struct SlotRefMut<'a, C: Capability> {
    pub(super) slot: SpinRwLockWriteGuard<'a, CapabilitySlotInner>,
    pub(super) _phantom: PhantomData<C>,
}

impl<'a, C: Capability> DerefMut for SlotRefMut<'a, C> {
    fn deref_mut(&'_ mut self) -> &'_ mut Self::Target {
        &mut self.slot
    }
}

impl<'a, C: Capability> Deref for SlotRefMut<'a, C> {
    type Target = CapabilitySlotInner;

    fn deref(&'_ self) -> &'_ Self::Target {
        &self.slot
    }
}

impl<'a> SlotRefMut<'a, AnyCap> {
    pub fn cap_type(&self) -> CapabilityType {
        self.slot.cap.cap_type()
    }

    pub fn downcast_mut<C: Capability>(self) -> Option<SlotRefMut<'a, C>> {
        if C::is_valid_type(self.cap.cap_type()) {
            Some(SlotRefMut {
                slot: self.slot,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn upcast_mut(self) -> SlotRefMut<'a, AnyCap> {
        SlotRefMut {
            slot: self.slot,
            _phantom: PhantomData,
        }
    }
}

// impls

impl<'a, C: Capability> fmt::Debug for SlotRef<'a, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotRef")
            .field("slot", &self.slot)
            .field("_phantom", &self._phantom)
            .finish()
    }
}

impl<'a, C: Capability> fmt::Debug for SlotRefMut<'a, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotRef")
            .field("slot", &self.slot)
            .field("_phantom", &self._phantom)
            .finish()
    }
}
