#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{arch::global_asm, fmt};

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

extern "C" fn thread_entry(_thread: Captr<Thread>) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    // _thread.suspend().unwrap();
    caps.thread.suspend().unwrap();
    RemoteCaptr::remote(caps.captbl, caps.thread)
        .delete()
        .unwrap();
    syscalls::debug::debug_dump_root();

    // syscalls::debug::debug_cap_slot(caps.captbl.into_raw(), caps.thread.into_raw()).unwrap();

    // println!("thread 2 waiting to recv");
    // println!("{:#x?}", unsafe {
    //     ecall1(SyscallNumber::NotificationWait, 65533)
    // });
    loop {
        unsafe {
            core::arch::asm!("nop");
        }
        // print!("\rthread 2!");
    }
}

static THREAD_STACK: &[u8; 1024 * 1024] = &[0; 1024 * 1024];

#[no_mangle]
extern "C" fn entry(init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    let _init_info = unsafe { &*init_info };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    syscalls::debug::debug_cap_slot(
        root_captbl.local_index().into_raw(),
        caps.allocator.into_raw(),
    )
    .unwrap();

    let thread: Captr<Thread> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65534) }, ())
        .unwrap();

    let _pg: Captr<Page<BasePage>> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65535) }, ())
        .unwrap();

    let notif: Captr<Notification> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65533) }, ())
        .unwrap();

    unsafe {
        ecall6(
            SyscallNumber::Grant,
            caps.captbl.into_raw() as u64,
            notif.into_raw() as u64,
            caps.captbl.into_raw() as u64,
            65532,
            CapRights::READ.bits(),
            0xDEADBEEF,
        )
        .unwrap();
    }

    syscalls::debug::debug_cap_slot(root_captbl.local_index().into_raw(), 65533).unwrap();
    syscalls::debug::debug_cap_slot(root_captbl.local_index().into_raw(), 65532).unwrap();

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
                a0: 65534,
                ..Default::default()
            })
            .unwrap()
    };

    syscalls::debug::debug_dump_root();

    unsafe { thread.resume().unwrap() }

    // for _ in 0..100_000_000 {
    //     unsafe {
    //         core::arch::asm!("pause");
    //     }
    // }

    // //unsafe { thread.resume().unwrap() }

    // for i in 0..100_000 {
    //     print!("\rthread 1 waiting to send: {i}");
    // }

    // unsafe { ecall1(SyscallNumber::NotificationSignal, 65532).unwrap() };
    // println!("\nthread 1 sent!");

    loop {
        // print!("\rthread 1!");
        unsafe {
            core::arch::asm!("nop");
        }
    }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
