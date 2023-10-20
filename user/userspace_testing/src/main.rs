#![no_std]
#![no_main]

use core::{arch::asm, panic::PanicInfo};

#[no_mangle]
fn _start() -> ! {
    let x = "Hello from userspace!\n";
    loop {
        unsafe {
            asm!("ecall", in("a0") 0, in("a1") x.as_ptr(), in("a2") x.len());
        }
    }
}

#[panic_handler]
fn panic(_panic: &PanicInfo<'_>) -> ! {
    unsafe {
        asm!("ecall", in("a0") 0, in("a1") "oops".as_ptr(), in("a2") 4);
    }
    loop {
        unsafe {
            asm!("nop");
        }
    }
}
