use core::{
    fmt,
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
};

use rille::capability::{CapResult, CapabilityType};

use crate::capability::RawCapability;

use super::{slotref::SlotRefMut, AnyCap, Capability, CapabilitySlot, EmptySlot};

#[derive(Copy, Clone, Default, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct DerivationTreeNode {
    pub(super) first_child: Option<NonNull<CapabilitySlot>>,
    pub(super) parent: Option<NonNull<CapabilitySlot>>,
    pub(super) next_sibling: Option<NonNull<CapabilitySlot>>,
    pub(super) prev_sibling: Option<NonNull<CapabilitySlot>>,
}

impl DerivationTreeNode {
    pub fn has_child(&self) -> bool {
        self.first_child.is_some()
    }

    pub fn first_child_mut(&'_ self) -> Option<SlotRefMut<'_, AnyCap>> {
        let first_child = self.first_child?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { first_child.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn next_sibling_mut(&'_ self) -> Option<SlotRefMut<'_, AnyCap>> {
        let next_sibling = self.next_sibling?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { next_sibling.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn prev_sibling_mut(&'_ self) -> Option<SlotRefMut<'_, AnyCap>> {
        let prev_sibling = self.prev_sibling?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { prev_sibling.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn parent_mut(&'_ self) -> Option<SlotRefMut<'_, AnyCap>> {
        let parent = self.parent?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { parent.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn has_child(&self) -> bool {
        self.slot.dtnode.first_child.is_some()
    }

    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    pub fn add_child<C2: Capability>(&mut self, child: &mut SlotRefMut<'_, C2>) {
        assert!(
            child.slot.dtnode.parent.is_none(),
            "SlotRefMut::add_child: parent was not null"
        );
        assert!(!ptr::eq(&*self.slot, &*child.slot));

        if let Some(next_sibling) = self.slot.dtnode.first_child {
            assert!(
                child.slot.dtnode.next_sibling.is_none(),
                "SlotRefMut::add_child: next_sibling was not null"
            );
            assert!(
                child.slot.dtnode.prev_sibling.is_none(),
                "SlotRefMut::add_child: prev_sibling was not null"
            );

            self.slot.dtnode.first_child =
                NonNull::new(ptr::addr_of!(*child.slot).cast_mut().cast());

            let self_addr = NonNull::new(ptr::addr_of!(*self.slot).cast_mut().cast());

            child.slot.dtnode.next_sibling = Some(next_sibling);
            child.slot.dtnode.parent = self_addr;

            child.next_sibling_mut().unwrap().slot.dtnode.prev_sibling =
                NonNull::new(ptr::addr_of!(*child.slot).cast_mut().cast());
        } else {
            self.slot.dtnode.first_child =
                NonNull::new(ptr::addr_of!(*child.slot).cast_mut().cast());
            child.slot.dtnode.parent = NonNull::new(ptr::addr_of!(*self.slot).cast_mut().cast());
        }
    }

    pub fn is_ancestor_of<C2: Capability>(&mut self, other: &mut SlotRefMut<'a, C2>) -> bool {
        let mut dtnode = other.slot.dtnode;

        loop {
            if let Some(parent) = dtnode.parent {
                if parent.as_ptr() == other.as_ptr().cast() {
                    break true;
                }
                // SAFETY: By invariants.
                let parent = unsafe { parent.as_ref() };
                dtnode = parent.lock.read().dtnode;
            } else {
                break false;
            }
        }
    }

    /// TODO
    ///
    /// # Errors
    ///
    /// TODO
    pub fn swap<C2: Capability>(
        mut self,
        mut other: SlotRefMut<'a, C2>,
    ) -> CapResult<(SlotRefMut<'a, C2>, SlotRefMut<'a, C>)> {
        // SAFETY: By invariants.
        let self_addr =
            unsafe { NonNull::new_unchecked(ptr::addr_of!(*self.slot).cast_mut().cast()) };
        // SAFETY: By invariants.
        let other_addr =
            unsafe { NonNull::new_unchecked(ptr::addr_of!(*other.slot).cast_mut().cast()) };
        let new_self_addr = other_addr;
        let new_other_addr = self_addr;

        let self_guard_addr = NonNull::new(0xFFFF_FFFF_FFFF_FFFF as *mut _);
        let other_guard_addr = NonNull::new(0xFFFF_FFFF_FFFF_FFFE as *mut _);

        let mut self_dtnode = self.slot.dtnode;
        let mut other_dtnode = other.slot.dtnode;

        if let Some(parent) = self_dtnode.parent {
            if parent == other_addr {
                if other_dtnode.first_child == Some(self_addr) {
                    other_dtnode.first_child = self_guard_addr;
                }
            } else {
                // SAFETY: By invariants.
                let parent = unsafe { parent.as_ref() };
                let mut lock = parent.lock.write();

                if lock.dtnode.first_child == Some(self_addr) {
                    lock.dtnode.first_child = self_guard_addr;
                }
            }
        }

        if let Some(parent) = other_dtnode.parent {
            if parent == self_addr || Some(parent) == self_guard_addr {
                if self_dtnode.first_child == Some(other_addr) {
                    self_dtnode.first_child = other_guard_addr;
                }
            } else {
                // SAFETY: By invariants.
                let parent = unsafe { parent.as_ref() };
                let mut lock = parent.lock.write();

                if lock.dtnode.first_child == Some(other_addr) {
                    lock.dtnode.first_child = other_guard_addr;
                }
            }
        }

        if let Some(prev_sibling) = self_dtnode.prev_sibling {
            if prev_sibling == other_addr || Some(prev_sibling) == other_guard_addr {
                other_dtnode.next_sibling = self_guard_addr;
            } else {
                // SAFETY: By invariants
                let prev_sibling = unsafe { prev_sibling.as_ref() };
                let mut lock = prev_sibling.lock.write();

                lock.dtnode.next_sibling = self_guard_addr;
            }
        }

        if let Some(prev_sibling) = other_dtnode.prev_sibling {
            if prev_sibling == self_addr || Some(prev_sibling) == self_guard_addr {
                self_dtnode.next_sibling = other_guard_addr;
            } else {
                // SAFETY: By invariants
                let prev_sibling = unsafe { prev_sibling.as_ref() };
                let mut lock = prev_sibling.lock.write();

                lock.dtnode.next_sibling = other_guard_addr;
            }
        }

        if let Some(next_sibling) = self_dtnode.next_sibling {
            if next_sibling == other_addr || Some(next_sibling) == other_guard_addr {
                other_dtnode.prev_sibling = self_guard_addr;
            } else {
                // SAFETY: By invariants
                let next_sibling = unsafe { next_sibling.as_ref() };
                let mut lock = next_sibling.lock.write();

                lock.dtnode.prev_sibling = self_guard_addr;
            }
        }

        if let Some(next_sibling) = other_dtnode.next_sibling {
            if next_sibling == self_addr || Some(next_sibling) == self_guard_addr {
                self_dtnode.prev_sibling = other_guard_addr;
            } else {
                // SAFETY: By invariants
                let next_sibling = unsafe { next_sibling.as_ref() };
                let mut lock = next_sibling.lock.write();

                lock.dtnode.prev_sibling = other_guard_addr;
            }
        }

        if let Some(first_child) = self_dtnode.first_child {
            let mut lock;
            let mut slot;
            if first_child == other_addr || Some(first_child) == other_guard_addr {
                slot = &mut other_dtnode;
            } else {
                // SAFETY: By invariants.
                let first_child = unsafe { first_child.as_ref() };
                lock = first_child.lock.write();
                slot = &mut lock.dtnode;
            };

            loop {
                slot.parent = self_guard_addr;

                if slot.next_sibling == Some(self_addr) || slot.next_sibling == self_guard_addr {
                    slot = &mut self_dtnode;
                } else if slot.next_sibling == Some(other_addr)
                    || slot.next_sibling == other_guard_addr
                {
                    slot = &mut other_dtnode;
                } else if let Some(next_sibling) = slot.next_sibling {
                    // SAFETY: By invariants.
                    let next_sibling = unsafe { next_sibling.as_ref() };
                    lock = next_sibling.lock.write();
                    slot = &mut lock.dtnode;
                } else {
                    break;
                }
            }
        }

        if let Some(first_child) = other_dtnode.first_child {
            let mut lock;
            let mut slot;
            if first_child == self_addr || Some(first_child) == self_guard_addr {
                slot = &mut self_dtnode;
            } else {
                // SAFETY: By invariants.
                let first_child = unsafe { first_child.as_ref() };
                lock = first_child.lock.write();
                slot = &mut lock.dtnode;
            };

            loop {
                slot.parent = other_guard_addr;

                if slot.next_sibling == Some(self_addr) || slot.next_sibling == self_guard_addr {
                    slot = &mut self_dtnode;
                } else if slot.next_sibling == Some(other_addr)
                    || slot.next_sibling == other_guard_addr
                {
                    slot = &mut other_dtnode;
                } else if let Some(next_sibling) = slot.next_sibling {
                    // SAFETY: By invariants.
                    let next_sibling = unsafe { next_sibling.as_ref() };
                    lock = next_sibling.lock.write();
                    slot = &mut lock.dtnode;
                } else {
                    break;
                }
            }
        }

        // Replace guard addresses with new addresses

        let mut new_self_dtnode = self_dtnode;
        let mut new_other_dtnode = other_dtnode;

        if let Some(parent) = self_dtnode.parent {
            if Some(parent) == other_guard_addr || parent == other_addr {
                if other_dtnode.first_child == self_guard_addr
                    || other_dtnode.first_child == Some(self_addr)
                {
                    new_other_dtnode.first_child = Some(new_self_addr);
                }
            } else {
                // SAFETY: By invariants.
                let parent = unsafe { parent.as_ref() };
                let mut lock = parent.lock.write();

                if lock.dtnode.first_child == self_guard_addr {
                    lock.dtnode.first_child = Some(new_self_addr);
                }
            }
        }

        if let Some(parent) = other_dtnode.parent {
            if Some(parent) == self_guard_addr || parent == self_addr {
                if self_dtnode.first_child == other_guard_addr
                    || self_dtnode.first_child == Some(other_addr)
                {
                    new_self_dtnode.first_child = Some(new_other_addr);
                }
            } else {
                // SAFETY: By invariants.
                let parent = unsafe { parent.as_ref() };
                let mut lock = parent.lock.write();

                if lock.dtnode.first_child == other_guard_addr {
                    lock.dtnode.first_child = Some(new_other_addr);
                }
            }
        }

        if let Some(prev_sibling) = self_dtnode.prev_sibling {
            if Some(prev_sibling) == other_guard_addr || prev_sibling == other_addr {
                new_other_dtnode.next_sibling = Some(new_self_addr);
            //} else if prev_sibling == other_addr {
            } else {
                // SAFETY: By invariants
                let prev_sibling = unsafe { prev_sibling.as_ref() };
                let mut lock = prev_sibling.lock.write();

                if lock.dtnode.next_sibling == self_guard_addr {
                    lock.dtnode.next_sibling = Some(new_self_addr);
                }
            }
        }

        if let Some(prev_sibling) = other_dtnode.prev_sibling {
            if Some(prev_sibling) == self_guard_addr || prev_sibling == other_addr {
                new_self_dtnode.next_sibling = Some(new_other_addr);
            //} else if prev_sibling == self_addr {
            } else {
                // SAFETY: By invariants
                let prev_sibling = unsafe { prev_sibling.as_ref() };
                let mut lock = prev_sibling.lock.write();

                if lock.dtnode.next_sibling == other_guard_addr {
                    lock.dtnode.next_sibling = Some(new_other_addr);
                }
            }
        }

        if let Some(next_sibling) = self_dtnode.next_sibling {
            if Some(next_sibling) == other_guard_addr || next_sibling == other_addr {
                new_other_dtnode.prev_sibling = Some(new_self_addr);
                new_self_dtnode.next_sibling = Some(new_other_addr);
            //} else if next_sibling == other_addr {
            } else {
                // SAFETY: By invariants
                let next_sibling = unsafe { next_sibling.as_ref() };
                let mut lock = next_sibling.lock.write();

                if lock.dtnode.prev_sibling == self_guard_addr {
                    lock.dtnode.prev_sibling = Some(new_self_addr);
                }
            }
        }

        if let Some(next_sibling) = other_dtnode.next_sibling {
            if Some(next_sibling) == self_guard_addr || next_sibling == self_addr {
                new_self_dtnode.prev_sibling = Some(new_other_addr);
            //} else if next_sibling == self_addr {
            } else {
                // SAFETY: By invariants
                let next_sibling = unsafe { next_sibling.as_ref() };
                let mut lock = next_sibling.lock.write();

                if lock.dtnode.prev_sibling == other_guard_addr {
                    lock.dtnode.prev_sibling = Some(new_other_addr);
                }
            }
        }

        if let Some(first_child) = self_dtnode.first_child {
            let mut lock;
            let mut slot;
            if Some(first_child) == other_guard_addr || first_child == other_addr {
                slot = &mut new_other_dtnode;
            } else {
                // SAFETY: By invariants.
                let first_child = unsafe { first_child.as_ref() };
                lock = first_child.lock.write();
                slot = &mut lock.dtnode;
            };

            loop {
                slot.parent = Some(new_self_addr);
                if slot.next_sibling == self_guard_addr || slot.next_sibling == Some(new_self_addr)
                {
                    slot = &mut new_self_dtnode;
                } else if slot.next_sibling == other_guard_addr
                    || slot.next_sibling == Some(new_other_addr)
                {
                    slot = &mut new_other_dtnode;
                } else if let Some(next_sibling) = slot.next_sibling {
                    // SAFETY: By invariants.
                    let next_sibling = unsafe { next_sibling.as_ref() };
                    lock = next_sibling.lock.write();
                    slot = &mut lock.dtnode;
                } else {
                    break;
                }
            }
        }

        if let Some(first_child) = other_dtnode.first_child {
            let mut lock;
            let mut slot;
            if Some(first_child) == self_guard_addr || first_child == self_addr {
                slot = &mut new_self_dtnode;
            } else {
                // SAFETY: By invariants.
                let first_child = unsafe { first_child.as_ref() };
                lock = first_child.lock.write();
                slot = &mut lock.dtnode;
            };

            loop {
                slot.parent = Some(new_other_addr);

                if slot.next_sibling == self_guard_addr || slot.next_sibling == Some(new_self_addr)
                {
                    slot = &mut new_self_dtnode;
                } else if slot.next_sibling == other_guard_addr
                    || slot.next_sibling == Some(new_other_addr)
                {
                    slot = &mut new_other_dtnode;
                } else if let Some(next_sibling) = slot.next_sibling {
                    // SAFETY: By invariants.
                    let next_sibling = unsafe { next_sibling.as_ref() };
                    lock = next_sibling.lock.write();
                    slot = &mut lock.dtnode;
                } else {
                    break;
                }
            }
        }

        if new_self_dtnode.first_child == other_guard_addr {
            new_self_dtnode.first_child = Some(new_other_addr);
        }
        if new_other_dtnode.first_child == self_guard_addr {
            new_other_dtnode.first_child = Some(new_self_addr);
        }

        if new_self_dtnode.parent == other_guard_addr {
            new_self_dtnode.parent = Some(new_other_addr);
        }
        if new_other_dtnode.parent == self_guard_addr {
            new_other_dtnode.parent = Some(new_self_addr);
        }

        if new_self_dtnode.next_sibling == other_guard_addr {
            new_self_dtnode.next_sibling = Some(new_other_addr);
        }
        if new_other_dtnode.next_sibling == self_guard_addr {
            new_other_dtnode.next_sibling = Some(new_self_addr);
        }

        if new_self_dtnode.prev_sibling == other_guard_addr {
            new_self_dtnode.prev_sibling = Some(new_other_addr);
        }
        if new_other_dtnode.prev_sibling == self_guard_addr {
            new_other_dtnode.prev_sibling = Some(new_self_addr);
        }

        mem::swap(&mut *self.slot, &mut *other.slot);

        self.slot.dtnode = new_other_dtnode;
        other.slot.dtnode = new_self_dtnode;

        let new_self = SlotRefMut {
            slot: self.slot,
            meta: other.meta,
            _phantom: PhantomData,
        };

        let new_other = SlotRefMut {
            slot: other.slot,
            meta: self.meta,
            _phantom: PhantomData,
        };

        Ok((new_self, new_other))
    }

    pub fn delete(mut self) -> SlotRefMut<'a, EmptySlot> {
        // SAFETY: By invariants.
        let self_addr =
            unsafe { NonNull::new_unchecked(ptr::addr_of!(*self.slot).cast_mut().cast()) };

        // SAFETY: This is the first time we do this, and we won't do
        // it again for this slot.
        unsafe { C::do_delete(self.meta, &mut self.slot) };

        let mut slot = SlotRefMut {
            slot: self.slot,
            meta: (),
            _phantom: PhantomData,
        };

        let next_sibling = slot.slot.dtnode.next_sibling;
        if let Some(mut parent) = slot.parent_mut() {
            if parent.slot.dtnode.first_child == Some(self_addr) {
                parent.slot.dtnode.first_child = next_sibling;
            }
        }

        if let Some(mut next_sibling) = slot.next_sibling_mut() {
            next_sibling.slot.dtnode.prev_sibling = None;
        }

        if let Some(mut prev_sibling) = slot.prev_sibling_mut() {
            prev_sibling.slot.dtnode.next_sibling = None;
        }

        slot.for_each_child_mut(|child| {
            child.slot.dtnode.parent = None;
        });
        slot.slot.dtnode = DerivationTreeNode::default();

        slot.slot.cap = RawCapability { empty: EmptySlot };
        slot.slot.cap_type = CapabilityType::Empty;

        slot
    }

    // TODO: make this an iterator again?
    pub fn for_each_child_mut(
        &mut self,
        mut f: impl for<'b, 'c> FnMut(&'c mut SlotRefMut<'b, AnyCap>),
    ) {
        if let Some(mut child) = self.first_child_mut() {
            f(&mut child);

            let mut slot = child;
            loop {
                let dtnode = slot.slot.dtnode;
                if dtnode.next_sibling.is_none() {
                    break;
                }

                // SAFETY: This value is Send + Sync. Additionally, the
                // pointer is non-null as we checked above. It's also valid by
                // invariants.
                let sibling = unsafe { dtnode.next_sibling.unwrap_unchecked().as_ref() };
                let sibling = sibling.lock.write();

                let meta = AnyCap::metadata_from_slot(&sibling);
                slot = SlotRefMut {
                    slot: sibling,
                    meta,
                    _phantom: PhantomData,
                };
                f(&mut slot);
            }
        }
    }

    pub fn first_child_mut(&'_ mut self) -> Option<SlotRefMut<'_, AnyCap>> {
        let dtnode = &self.slot.dtnode;

        let first_child = dtnode.first_child?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { first_child.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn next_sibling_mut(&'_ mut self) -> Option<SlotRefMut<'_, AnyCap>> {
        let dtnode = &self.slot.dtnode;

        let next_sibling = dtnode.next_sibling?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { next_sibling.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn prev_sibling_mut(&'_ mut self) -> Option<SlotRefMut<'_, AnyCap>> {
        let dtnode = &self.slot.dtnode;

        let prev_sibling = dtnode.prev_sibling?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { prev_sibling.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }

    pub fn parent_mut(&'_ mut self) -> Option<SlotRefMut<'_, AnyCap>> {
        let dtnode = &self.slot.dtnode;

        let parent = dtnode.parent?;

        // SAFETY: This value is Send + Sync. Additionally, the
        // pointer is non-null as we checked above. It's also valid by
        // invariants.
        let slot = unsafe { parent.as_ref() };
        let slot = slot.lock.write();

        let meta = AnyCap::metadata_from_slot(&slot);
        Some(SlotRefMut {
            slot,
            meta,
            _phantom: PhantomData,
        })
    }
}

// impls

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
