#![no_std]
#![no_main]
#![feature(maybe_uninit_slice)]

use core::{arch::global_asm, mem::MaybeUninit};

use rille::{
    capability::{Captr, Endpoint},
    syscalls,
};
use rille_user::{malloc::LinkedListAlloc, sync::once_cell::OnceCell};

global_asm!(
    "
.pushsection .init
.option norvc
.type _start, @function
.global _start
_start:
.option push
.option norelax
    lla gp, __global_pointer$
.option pop

    lla t0, __bss_start
    lla t1, __bss_end
    // Clear the .bss section
clear_bss:
    beq t0, t1, done_clear_bss
    sd zero, (t0)
    addi t0, t0, 8
    j clear_bss

done_clear_bss:
    lla sp, __stack_top
    tail entry
.popsection
"
);

#[global_allocator]
static GLOBAL_ALLOC: OnceCell<LinkedListAlloc> = OnceCell::new();

#[no_mangle]
unsafe extern "C" fn entry(ep: Endpoint, n_pages: usize) -> ! {
    let mut job = Captr::null();

    ep.recv_with_regs(Some(&mut job)).unwrap();

    syscalls::debug::debug_dump_root();
    syscalls::debug::debug_cap_slot(job.into_raw()).unwrap();

    loop {
        core::arch::asm!("nop");
    }
}

#[panic_handler]
unsafe fn panic_handler(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::arch::asm!("nop")
    }
}

struct DebugPrint;

impl core::fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
