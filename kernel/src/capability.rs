use core::{marker::PhantomData, mem::MaybeUninit};

use crate::{
    addr::{DirectMapped, VirtualMut},
    proc::Context,
    spin::SpinMutex,
    trampoline::Trapframe,
};

pub struct Captbl {
    radix: usize,
    data: *mut Capability,
    _phantom: PhantomData<Capability>,
}

pub enum Capability {
    Null,
    Captbl(*mut Captbl),
    Thread(*mut Thread),
    PgTbl(*mut PgTbl),
    Untyped(Untyped),
}

pub enum CapabilityType {
    Captbl,
    Thread,
    PgTbl,
    Untyped,
}

impl Default for Capability {
    fn default() -> Self {
        Self::Null
    }
}

#[repr(C)]
pub struct Thread {
    trapframe: Trapframe,
    captbl: *mut Captbl,
    vspace: *mut PgTbl,
    kernel_stack: VirtualMut<u8, DirectMapped>,
    context: SpinMutex<Context>,
}

pub struct PgTbl {}

pub struct Untyped {
    len: usize,
    next: usize,
    data: *mut u8,
}

impl Untyped {
    #[must_use]
    pub fn retype_untyped(&mut self, size: usize) -> Untyped {
        assert!(
            size.is_power_of_two(),
            "Untyped::retype_untyped: size must be a power of two"
        );

        let unaligned_offset = self.next % size;
        let until_next_align = size - unaligned_offset;
        let new_next = self.next + until_next_align;
        debug_assert_eq!(new_next % size, 0);

        assert!(
            self.len - self.next >= new_next,
            "Untyped::retype_untyped: not enough space"
        );

        self.next = new_next + size;

        Untyped {
            len: size,
            next: new_next,
            data: unsafe { self.data.add(new_next) },
        }
    }
    // pub fn retype(
    //     &mut self,
    //     ty: CapabilityType,
    //     size: usize,
    //     offset: usize,
    //     num: usize,
    // ) {
    //     match ty {
    //         CapabilityType::Untyped => {
    //             todo!()
    //         }
    //         _ => todo!(),
    //     }
    // }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Captr(u64);

impl Captbl {
    #[inline(always)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        1 << self.radix
    }

    pub fn get_mut(&mut self, index: usize) -> &mut Capability {
        assert!(index < self.len());
        unsafe { &mut *self.data.add(index) }
    }

    pub fn get(&self, index: usize) -> &Capability {
        assert!(index < self.len());
        unsafe { &*self.data.add(index) }
    }

    pub unsafe fn walk_mut(&mut self, captr: Captr, depth_limit: u8) -> &mut Capability {
        let captr = captr.0;
        let mut captbl: &mut Self = self;
        let mut radix = captbl.radix;
        let mut bits_left = depth_limit;
        loop {
            let current_idx = captr >> (64 - radix);
            let ptr = unsafe { captbl.data.add(current_idx as usize) };
            match unsafe { &mut *ptr } {
                Capability::Captbl(tbl) if bits_left > 0 => {
                    captbl = unsafe { &mut **tbl };
                }
                cap => return cap,
            }
            bits_left -= radix as u8;
        }
    }
}
