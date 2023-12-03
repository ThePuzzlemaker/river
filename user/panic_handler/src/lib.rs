#![no_std]
#![feature(panic_info_message)]

use core::{arch::asm, fmt, panic::PanicInfo};

use rille::syscalls;

macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	syscalls::debug::debug_print_string("\n")
    }}
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}

fn tp() -> u64 {
    let x: u64;
    unsafe {
        asm!("mv {}, tp", out(reg) x, options(nomem));
    }
    x
}

#[panic_handler]
unsafe fn panic(panic: &PanicInfo<'_>) -> ! {
    if let Some(msg) = panic.message() {
        println!("panic occurred (thread {}): {}", tp(), msg);
        if let Some(location) = panic.location() {
            println!(
                "  at {}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        }
    }

    loop {
        asm!("pause");
    }
}
