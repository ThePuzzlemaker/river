use core::{
    fmt,
    marker::PhantomData,
    mem::{self, ManuallyDrop, MaybeUninit},
    ptr::{self},
};

use alloc::sync::{Arc, Weak};
use itertools::Itertools;
use rille::{
    addr::{DirectMapped, VirtualMut},
    capability::{self as rille_cap},
};

use crate::kalloc::{self, phys::PMAlloc};

use super::{CapToOwned, Capability, CapabilitySlot, CaptblHeader, SlotRef, SlotRefMut};

pub union CaptblInner {
    pub(super) slot: ManuallyDrop<CapabilitySlot>,
    pub(super) hdr: CaptblHeader,
}

#[derive(Clone)]
#[repr(transparent)]
pub struct Captbl {
    pub(super) inner: Arc<UnsafeCaptblPtr>,
}

#[derive(Clone)]
#[repr(transparent)]
pub(super) struct UnsafeCaptblPtr(pub(super) *const [CaptblInner]);

// SAFETY: The user of this type ensures that any accesses to the
// inner value are properly synchronized.
unsafe impl Send for UnsafeCaptblPtr {}
// SAFETY: See above.
unsafe impl Sync for UnsafeCaptblPtr {}

impl Captbl {
    pub fn downgrade(&self) -> WeakCaptbl {
        WeakCaptbl {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

#[derive(Clone)]
#[repr(transparent)]
pub struct WeakCaptbl {
    pub(super) inner: Weak<UnsafeCaptblPtr>,
}

impl WeakCaptbl {
    pub fn upgrade(&self) -> Option<Captbl> {
        Some(Captbl {
            inner: Weak::upgrade(&self.inner)?,
        })
    }
}

impl Drop for Captbl {
    fn drop(&mut self) {
        // If we're the last of this Arc, deallocate the captbl, since
        // Arc's drop won't do anything for pointers.
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            let order = {
                // SAFETY: By our invariants, the header must be
                // initialized and this memory should still be valid.
                let hdr = unsafe { &*inner.0 };
                // SAFETY: By our invariants, the 0th element must be
                // the header, and must be valid and initialized.
                let n_slots_log2 = unsafe { hdr[0].hdr }.n_slots_log2;
                kalloc::phys::what_order(1 << (n_slots_log2 + 6))
            };
            let ptr_raw = inner.0.as_ptr().cast_mut();
            let ptr_virt = VirtualMut::from_ptr(ptr_raw);
            {
                let mut pma = PMAlloc::get();
                // SAFETY: By our invariants, this memory was
                // allocated using this order and the pointer is
                // valid.
                unsafe { pma.deallocate(ptr_virt.into_phys().cast(), order) }
            }
        }
    }
}

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
    pub unsafe fn new(base: VirtualMut<u8, DirectMapped>, n_slots_log2: u8) -> Self {
        let header = CaptblHeader { n_slots_log2 };

        let captbl: *mut [MaybeUninit<CaptblInner>] =
            ptr::slice_from_raw_parts_mut(base.into_ptr_mut().cast(), 1 << n_slots_log2);
        // SAFETY: This memory is valid, and it is valid to read it as
        // MaybeUninit even if it's uninitialized.
        let captbl = unsafe { &mut *captbl };
        captbl[0].write(CaptblInner { hdr: header });

        for slot in captbl.iter_mut().skip(1) {
            slot.write(CaptblInner {
                slot: ManuallyDrop::new(CapabilitySlot::default()),
            });
        }

        // SAFETY: We have just initialized this slice.
        let captbl = unsafe { MaybeUninit::slice_assume_init_mut(captbl) };
        let captbl = ptr::addr_of!(*captbl);

        Self {
            inner: Arc::new(UnsafeCaptblPtr(captbl)),
        }
    }
}

#[repr(transparent)]
#[derive(Clone, PartialEq, Eq)]
pub struct CaptblSlot {
    weak: WeakCaptbl,
}

// === impls ===

struct CaptblSlots<'a>(&'a [CaptblInner]);

struct DebugAdapterCoalesce<'a>(u64, &'a CapabilitySlot);

impl<'a> fmt::Debug for DebugAdapterCoalesce<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.1.fmt(f)?;
        match self.0 {
            0 => Ok(()),
            n => write!(f, " <repeated {} times>", n + 1),
        }
    }
}

impl<'a> fmt::Debug for CaptblSlots<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.0
                    .iter()
                    .skip(1)
                    // SAFETY: By invariants
                    .map(|x| DebugAdapterCoalesce(0, unsafe { &x.slot }))
                    .coalesce(|l, r| {
                        if *l.1.lock.read() == *r.1.lock.read() {
                            Ok(DebugAdapterCoalesce(l.0 + 1, l.1))
                        } else {
                            Err((l, r))
                        }
                    })
                    .map(|x| (x.1 as *const _, x)),
            )
            .finish()
    }
}

impl fmt::Debug for Captbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Captbl")
            .field("inner", &self.inner.0)
            // SAFETY: By invariants.
            .field("hdr", unsafe { &(*self.inner.0)[0].hdr })
            // SAFETY: By invariants
            .field("table", &CaptblSlots(unsafe { &*self.inner.0 }))
            .finish()
    }
}

impl fmt::Debug for WeakCaptbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(captbl) = self.upgrade() {
            f.debug_struct("WeakCaptbl")
                .field("inner", &captbl.inner.0)
                .finish()
        } else {
            f.debug_struct("WeakCaptbl")
                .field("inner", &"Weak")
                .finish()
        }
    }
}

impl PartialEq for WeakCaptbl {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for WeakCaptbl {}

impl PartialEq for Captbl {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Captbl {}

impl<'a> CapToOwned for SlotRef<'a, rille_cap::Captbl> {
    type Target = WeakCaptbl;

    fn to_owned_cap(&self) -> Self::Target {
        self.cap.captbl().unwrap().clone()
    }
}

impl<'a> CapToOwned for SlotRefMut<'a, rille_cap::Captbl> {
    type Target = WeakCaptbl;

    fn to_owned_cap(&self) -> Self::Target {
        self.cap.captbl().unwrap().clone()
    }
}

impl fmt::Debug for CaptblSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CaptblSlot").field(&self.weak).finish()
    }
}

impl<'a, C: Capability> SlotRefMut<'a, C> {
    pub fn swap<C2: Capability>(
        mut self,
        mut other: SlotRefMut<'a, C2>,
    ) -> (SlotRefMut<'a, C2>, SlotRefMut<'a, C>) {
        mem::swap(&mut *self.slot, &mut *other.slot);

        let new_self = SlotRefMut {
            slot: self.slot,
            _phantom: PhantomData,
        };
        let new_other = SlotRefMut {
            slot: other.slot,
            _phantom: PhantomData,
        };

        (new_self, new_other)
    }
}
