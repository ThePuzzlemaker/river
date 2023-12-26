#![no_std]

use rille::capability::{Captr, MessageHeader};

extern crate rt;

fn main() {
    core::fmt::write(&mut DebugPrint, format_args!("{:#?}\n", rt::procsvr())).unwrap();
    rt::procsvr()
        .send_with_regs(
            MessageHeader::new().with_private(0xFF),
            Captr::null(),
            [0; 4],
        )
        .unwrap();
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

struct DebugPrint;

impl core::fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        rille::syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
