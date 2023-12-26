#![no_std]
#![allow(internal_features)]
#![feature(lang_items, naked_functions)]

use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use rille::capability::{Capability, Captr, Endpoint};

static PROCSVR: AtomicUsize = AtomicUsize::new(0);

#[naked]
#[link_section = ".rt.entry"]
#[no_mangle]
unsafe extern "C" fn _start() -> ! {
    core::arch::asm!(
        "
.option push
.option norelax
    lla gp, __global_pointer$
.option pop

    lla t0, __bss_start
    lla t1, end
    // Clear the .bss section
1:
    beq t0, t1, 2f
    sd zero, (t0)
    addi t0, t0, 8
    j 1b

2:
    tail _rust_start
",
        options(noreturn)
    );
}

#[lang = "start"]
fn lang_start<T>(main: fn() -> T, _argc: isize, _argv: *const *const u8, _: u8) -> isize {
    (main)();
    0
}

#[no_mangle]
#[link_section = ".rt.entry"]
unsafe extern "C" fn _rust_start(a0: usize) -> ! {
    extern "C" {
        fn main(_: isize, _: *const *const u8) -> isize;
    }
    PROCSVR.store(a0, Ordering::Relaxed);

    main(0, ptr::null_mut());

    // TODO: exit
    loop {
        core::arch::asm!("nop")
    }
}

pub fn procsvr() -> Endpoint {
    Endpoint::from_captr(Captr::from_raw(PROCSVR.load(Ordering::Relaxed)))
}
