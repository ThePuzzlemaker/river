use core::arch::global_asm;

global_asm!(
    "
// Switch from user-space into the kernel. This must be mapped at the same
// virtual address in U-mode and S-mode, and must be page-aligned.
.pushsection .init.trampoline

.option norvc

.type trampoline, @function
.global trampoline
trampoline:
    // N.B. we start from U-mode, but are now in S-mode (but with a U-mode page
    // table).

    // Save the user's t0 so it can be used to index into the trapframe.
    csrw sscratch, t0

    // Load the address of the trapframe into t0 
    li t0, 0xffffffffffffefff

    sd x1,    0(t0)
    sd x2,    8(t0)
    sd x3,   16(t0)
    sd x4,   24(t0)
    // N.B. x5 == t0
    sd x6,   40(t0)
    sd x7,   48(t0)
    sd x8,   56(t0)
    sd x9,   64(t0)
    sd x10,  72(t0)
    sd x11,  80(t0)
    sd x12,  88(t0)
    sd x13,  96(t0)
    sd x14, 104(t0)
    sd x15, 112(t0)
    sd x16, 120(t0)
    sd x17, 128(t0)
    sd x18, 136(t0)
    sd x19, 144(t0)
    sd x20, 152(t0)
    sd x21, 160(t0)
    sd x22, 168(t0)
    sd x23, 176(t0)
    sd x24, 184(t0)
    sd x25, 192(t0)
    sd x26, 200(t0)
    sd x27, 208(t0)
    sd x28, 216(t0)
    sd x29, 224(t0)
    sd x30, 232(t0)
    sd x31, 240(t0)

    // Save the user's x1 (that we stored in sscratch) into the trapframe,
    // ensuring we don't clobber the x1 address we need (we can use x2, as we
    // already saved it).
    csrr t1, sscratch
    sd t1, 32(t0)

    ld sp, 248(t0)
    ld tp, 256(t0)
    // Address of user trap handler
    ld t1, 264(t0)
    // S-mode SATP
    ld t2, 272(t0)

    sfence.vma

    csrw satp, t2

    sfence.vma

    // Jump to the user trap handler.
    //
    // N.B. We need to keep the trap-related values (stvec, sepc, etc.) and I
    // don't want to have to store those and swap them out later, so we don't
    // use the same method to get to the user mode trap handler that we do to
    // get to kmain (specifically, setting stvec and `unimp`) 
    j t1

.type ret_user, @function
.global ret_user
ret_user:
    // Switch to U-mode from S-mode.
    //
    // N.B. a0 = U-mode satp
    
    sfence.vma
    csrw satp, a0
    sfence.vma

    // Load the address of the trapframe into t0 
    li t0, 0xffffffffffffefff

    ld x1,    0(t0)
    ld x2,    8(t0)
    ld x3,   16(t0)
    ld x4,   24(t0)
    // N.B. x5 == t0
    ld x6,   40(t0)
    ld x7,   48(t0)
    ld x8,   56(t0)
    ld x9,   64(t0)
    ld x10,  72(t0)
    ld x11,  80(t0)
    ld x12,  88(t0)
    ld x13,  96(t0)
    ld x14, 104(t0)
    ld x15, 112(t0)
    ld x16, 120(t0)
    ld x17, 128(t0)
    ld x18, 136(t0)
    ld x19, 144(t0)
    ld x20, 152(t0)
    ld x21, 160(t0)
    ld x22, 168(t0)
    ld x23, 176(t0)
    ld x24, 184(t0)
    ld x25, 192(t0)
    ld x26, 200(t0)
    ld x27, 208(t0)
    ld x28, 216(t0)
    ld x29, 224(t0)
    ld x30, 232(t0)
    ld x31, 240(t0)

    ld t0, 32(t0)

    // Return to U-mode. Our caller must have set stval and made sure SPP is
    // off.
    sret

.popsection
"
);

extern "C" {
    pub fn trampoline();
    pub fn user_ret();
}

#[repr(C)]
pub struct Trapframe {
    /*   0 */ pub x1: u64,
    /*   8 */ pub x2: u64,
    /*  16 */ pub x3: u64,
    /*  24 */ pub x4: u64,
    /*  32 */ pub x5: u64,
    /*  40 */ pub x6: u64,
    /*  48 */ pub x7: u64,
    /*  56 */ pub x8: u64,
    /*  64 */ pub x9: u64,
    /*  72 */ pub x10: u64,
    /*  80 */ pub x11: u64,
    /*  88 */ pub x12: u64,
    /*  96 */ pub x13: u64,
    /* 104 */ pub x14: u64,
    /* 112 */ pub x15: u64,
    /* 120 */ pub x16: u64,
    /* 128 */ pub x17: u64,
    /* 136 */ pub x18: u64,
    /* 144 */ pub x19: u64,
    /* 152 */ pub x20: u64,
    /* 160 */ pub x21: u64,
    /* 168 */ pub x22: u64,
    /* 176 */ pub x23: u64,
    /* 184 */ pub x24: u64,
    /* 192 */ pub x25: u64,
    /* 200 */ pub x26: u64,
    /* 208 */ pub x27: u64,
    /* 216 */ pub x28: u64,
    /* 224 */ pub x29: u64,
    /* 232 */ pub x30: u64,
    /* 240 */ pub x31: u64,
    /* 248 */ pub kernel_sp: u64,
    /* 256 */ pub kernel_tp: u64,
    /* 264 */ pub kernel_trap: u64,
    /* 272 */ pub kernel_satp: u64,
    /* 280 */ pub user_epc: u64,
}
