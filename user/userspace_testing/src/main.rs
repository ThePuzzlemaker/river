#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{
    arch::global_asm,
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use rille::{
    capability::{
        paging::{BasePage, Page},
        CapRights, Captr, Notification, RemoteCaptr, Thread,
    },
    init::{BootInfo, InitCapabilities},
    syscalls::{self, ecall1, ecall6, SyscallNumber},
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

extern "C" fn thread_entry(
    _thread: Captr<Thread>,
    in_notif: Captr<Notification>,
    out_notif: Captr<Notification>,
) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    // _thread.suspend().unwrap();
    // caps.thread.suspend().unwrap();
    // RemoteCaptr::remote(caps.captbl, caps.thread)
    //     .delete()
    //     .unwrap();
    // syscalls::debug::debug_dump_root();

    // syscalls::debug::debug_cap_slot(caps.captbl.into_raw(), caps.thread.into_raw()).unwrap();

    // println!("\nthread 2 waiting to recv");
    // println!("{:#x?}", unsafe {
    //     Captr::from_raw_unchecked(65533).wait().unwrap()
    // });

    println!("{:#?}", time());
    out_notif.signal().unwrap();

    while in_notif.wait().is_ok() {
        let n = SHARED.fetch_add(1, Ordering::Relaxed);

        //print!("\rpong!: {}       {:?}     ", n, time());

        if n == 50_000 {
            break;
        }
        out_notif.signal().unwrap()
    }

    loop {
        unsafe {
            core::arch::asm!("nop");
        }
    }

    // loop {
    //     unsafe {
    //         core::arch::asm!("nop");
    //     }
    //     // print!("\rthread 2!");
    // }
}

static SHARED: AtomicU64 = AtomicU64::new(0);
static THREAD_STACK: &[u8; 1024 * 1024] = &[0; 1024 * 1024];

pub fn time() -> (u64, u64) {
    let timebase_freq = 10_000_000;
    let time: u64;

    unsafe { core::arch::asm!("csrr {}, time", out(reg) time, options(nostack)) };

    let time_ns = time * (1_000_000_000 / timebase_freq);
    let sec = time_ns / 1_000_000_000;
    let time_ns = time_ns % 1_000_000_000;
    let ms = time_ns / 1_000_000;
    (sec, ms)
}

#[no_mangle]
extern "C" fn entry(init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    let _init_info = unsafe { &*init_info };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    let thread: Captr<Thread> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65534) }, ())
        .unwrap();

    let _pg: Captr<Page<BasePage>> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65535) }, ())
        .unwrap();

    let mut notifs = caps
        .allocator
        .allocate_many(
            root_captbl,
            unsafe { Captr::from_raw_unchecked(65531) },
            2,
            (),
        )
        .unwrap();

    let out_notif = notifs.next().unwrap();
    let in_notif = notifs.next().unwrap();

    unsafe {
        ecall6(
            SyscallNumber::Grant,
            caps.captbl.into_raw() as u64,
            out_notif.into_raw() as u64,
            caps.captbl.into_raw() as u64,
            out_notif.into_raw() as u64,
            CapRights::all().bits(),
            0xDEAD0000,
        )
        .unwrap();
    }

    unsafe {
        ecall6(
            SyscallNumber::Grant,
            caps.captbl.into_raw() as u64,
            in_notif.into_raw() as u64,
            caps.captbl.into_raw() as u64,
            in_notif.into_raw() as u64,
            CapRights::all().bits(),
            0xDEAD0000,
        )
        .unwrap();
    }

    // unsafe { Captr::<Page<BasePage>>::from_raw_unchecked(65533) }
    //     .map(
    //         caps.pgtbl,
    //         VirtualConst::from_usize(0xDEAD0000),
    //         PageTableFlags::RW,
    //     )
    //     .unwrap();

    // unsafe {
    //     core::ptr::write_volatile(0xdead0000 as *mut u64, 0xc0ded00d);
    // }

    unsafe { thread.configure(Captr::null(), Captr::null()).unwrap() };
    unsafe {
        thread
            .write_registers(&rille::capability::UserRegisters {
                pc: thread_entry as usize as u64,
                sp: THREAD_STACK.as_ptr().wrapping_add(1024 * 1024) as usize as u64,
                a0: thread.into_raw() as u64,
                a1: out_notif.into_raw() as u64,
                a2: in_notif.into_raw() as u64,
                ..Default::default()
            })
            .unwrap()
    };

    syscalls::debug::debug_dump_root();

    //unsafe { thread.resume().unwrap() }

    unsafe { thread.resume().unwrap() }

    while in_notif.wait().is_ok() {
        let n = SHARED.fetch_add(1, Ordering::Relaxed);

        //        print!("\rping!: {}       {:?}     ", n, time());
        if n == 50_000 {
            println!("\ndone! {:#?}", time());
            break;
        }
        out_notif.signal().unwrap();
    }

    syscalls::debug::debug_dump_root();

    loop {
        unsafe {
            core::arch::asm!("nop");
        }
    }

    // println!("\nthread 1 sent!");

    // loop {
    //     // print!("\rthread 1!");
    //     unsafe {
    //         core::arch::asm!("nop");
    //     }
    // }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
