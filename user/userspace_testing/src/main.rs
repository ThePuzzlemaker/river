#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{arch::global_asm, fmt};

use rille::{
    capability::{Captr, RemoteCaptr, Thread},
    init::{BootInfo, InitCapabilities},
    syscalls::{self},
};

extern crate panic_handler;

#[allow(unused_macros)]
macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	syscalls::debug::debug_print_string("\n")
    }}
}

#[allow(unused_macros)]
macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
    }}
}

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

extern "C" fn thread_entry(_thread: Captr<Thread>) -> ! {
    let _caps = unsafe { InitCapabilities::new() };
    loop {
        print!("\rthread 2!");
    }
}

static THREAD_STACK: &[u8; 1024 * 1024] = &[0; 1024 * 1024];

#[no_mangle]
extern "C" fn entry(init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    let _init_info = unsafe { &*init_info };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    let thread: Captr<Thread> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65534) }, ())
        .unwrap();
    unsafe { thread.configure(Captr::null(), Captr::null()).unwrap() };
    unsafe {
        thread
            .write_registers(&rille::capability::UserRegisters {
                pc: thread_entry as usize as u64,
                sp: THREAD_STACK.as_ptr().wrapping_add(1024 * 1024) as usize as u64,
                a0: 65534,
                ..Default::default()
            })
            .unwrap()
    };

    unsafe { thread.resume().unwrap() }

    loop {
        print!("\rthread 1!");
    }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
