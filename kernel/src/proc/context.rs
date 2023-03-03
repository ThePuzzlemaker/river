use core::{arch::global_asm, mem, sync::atomic::AtomicU64};

use crate::spin::SpinMutex;

global_asm!(
    "
.pushsection .text
.globl context_switch
.type context_switch, @function
context_switch:
    sd ra,    0(a0)
    sd sp,    8(a0)
    sd s0,   16(a0)
    sd s1,   24(a0)
    sd s2,   32(a0)
    sd s3,   40(a0)
    sd s4,   48(a0)
    sd s5,   56(a0)
    sd s6,   64(a0)
    sd s7,   72(a0)
    sd s8,   80(a0)
    sd s9,   88(a0)
    sd s10,  96(a0)
    sd s11, 104(a0)

    sd zero, 0(a2)
    fence

    ld ra,    0(a1)
    ld sp,    8(a1)
    ld s0,   16(a1)
    ld s1,   24(a1)
    ld s2,   32(a1)
    ld s3,   40(a1)
    ld s4,   48(a1)
    ld s5,   56(a1)
    ld s6,   64(a1)
    ld s7,   72(a1)
    ld s8,   80(a1)
    ld s9,   88(a1)
    ld s10,  96(a1)
    ld s11, 104(a1)

    sd zero, 0(a3)
    fence

    ret
.popsection
"
);

extern "C" {
    fn context_switch(
        old: *mut Context,
        new: *mut Context,
        old_lock: *const AtomicU64,
        new_lock: *const AtomicU64,
    );
}

/// Register spill area for kernel context switches.
///
/// N.B. We don't need to store tx and ax registers, as we are in the
/// kernel when these are swapped, so our we only must save
/// callee-saved registers (sx)
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Context {
    /*   0 */ pub ra: u64,
    /*   8 */ pub sp: u64,
    /*  16 */ pub s0: u64,
    /*  24 */ pub s1: u64,
    /*  32 */ pub s2: u64,
    /*  40 */ pub s3: u64,
    /*  48 */ pub s4: u64,
    /*  56 */ pub s5: u64,
    /*  64 */ pub s6: u64,
    /*  72 */ pub s7: u64,
    /*  80 */ pub s8: u64,
    /*  88 */ pub s9: u64,
    /*  96 */ pub s10: u64,
    /* 104 */ pub s11: u64,
}

impl Context {
    /// Perform a context switch, switching into `dst` from `src`.
    ///
    /// # Safety
    ///
    /// Both contexts should be appropriately set up so that switching
    /// into them does not cause memory unsafety or other undefined
    /// behaviour.
    ///
    /// # Deadlock Safety
    ///
    /// Both contexts must be distinct otherwise deadlock will occur.
    pub unsafe fn switch(dst: &SpinMutex<Context>, src: &SpinMutex<Context>) {
        let dst_guard = dst.lock();
        let src_guard = src.lock();

        // Lock the contexts, but forget the lock existed, so that
        // they can be used within `context_switch` but do not get
        // spuriously unlocked after we've switched back here.
        mem::forget((dst_guard, src_guard));

        let (dst, dst_lock) = dst.to_components();
        let (src, src_lock) = src.to_components();

        // SAFETY: Both contexts are locked and our caller guarantees
        // they are valid.
        unsafe {
            context_switch(src, dst, src_lock, dst_lock);
        }
    }
}
