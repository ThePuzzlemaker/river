#![no_std]
#![no_main]

use core::arch::global_asm;

use rille::syscalls;
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
unsafe extern "C" fn entry() -> ! {
    syscalls::debug::debug_dump_root();
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
