#![no_std]

use core::ptr;

use rille::capability::Endpoint;
use rille_user::sync::once_cell::OnceCell;

extern crate rt;

fn main() {
    unsafe {
        rille::syscalls::debug::debug_print_string("hello world\n");

        loop {
            core::arch::asm!("nop")
        }
    }
}

//TODO
#[panic_handler]
unsafe fn panic_handler(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::arch::asm!("nop")
    }
}
