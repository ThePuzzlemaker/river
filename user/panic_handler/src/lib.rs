#![no_std]
#![feature(panic_info_message)]

use core::{arch::asm, fmt, panic::PanicInfo, ptr};

macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	print("\n")
    }}
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print(s);
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

// TODO
pub fn print(s: &str) {
    let serial = 0x80000000 as *mut u8;
    for b in s.as_bytes() {
        // SAFETY: The invariants of the serial driver ensure this is valid.
        while (unsafe { ptr::read_volatile(serial.add(5)) } & (1 << 5)) == 0 {
            core::hint::spin_loop();
        }
        // SAFETY: See above.
        unsafe { ptr::write_volatile(serial, *b) }
    }
}
