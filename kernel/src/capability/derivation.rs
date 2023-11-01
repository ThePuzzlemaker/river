use core::{fmt, marker::PhantomData, ptr};

use rille::addr::Virtual;

use super::{captbl::Captbl, slotref::SlotRefMut, Capability, SlotPtrWithTable};

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct DerivationTreeNode {
    first_child: SlotPtrWithTable,
    parent: SlotPtrWithTable,
    next_sibling: SlotPtrWithTable,
    prev_sibling: SlotPtrWithTable,
    _pad0: u64,
    _pad1: u64,
}

const _ASSERT_DTNODE_SIZE_EQ_48: () = assert!(
    core::mem::size_of::<DerivationTreeNode>() == 48,
    "NullCapSlot size was not 48 bytes"
);

impl<'a, C: Capability> SlotRefMut<'a, C> {
    /// TODO
    ///
    /// # Panics
    ///
    /// TODO
    pub fn add_child(&mut self, child: &mut SlotRefMut<'a, C>) {
        assert_eq!(
            child.slot.dtnode.parent,
            SlotPtrWithTable::null(),
            "SlotRefMut::add_child: parent was not 0"
        );
        assert!(!ptr::eq(self.slot, child.slot));
        if self.slot.dtnode.first_child == SlotPtrWithTable::null() {
            self.slot.dtnode.first_child =
                SlotPtrWithTable::new(ptr::addr_of!(*child.slot), child.tbl_addr.into_ptr());
            child.slot.dtnode.parent =
                SlotPtrWithTable::new(ptr::addr_of!(*self.slot), self.tbl_addr.into_ptr());
        } else {
            assert_eq!(
                child.slot.dtnode.next_sibling,
                SlotPtrWithTable::null(),
                "SlotRefMut::add_child: next_sibling was not 0"
            );
            assert_eq!(
                child.slot.dtnode.prev_sibling,
                SlotPtrWithTable::null(),
                "SlotRefMut::add_child: prev_sibling was not 0"
            );
            let next_sibling = self.slot.dtnode.first_child;
            self.slot.dtnode.first_child =
                SlotPtrWithTable::new(ptr::addr_of!(*child.slot), child.tbl_addr.into_ptr());
            let self_addr =
                SlotPtrWithTable::new(ptr::addr_of!(*self.slot), self.tbl_addr.into_ptr());
            child.slot.dtnode.next_sibling = next_sibling;
            child.slot.dtnode.parent = self_addr;
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

        if dtnode.next_sibling == SlotPtrWithTable::null() {
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

        if dtnode.parent == SlotPtrWithTable::null() {
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
