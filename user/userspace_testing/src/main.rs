#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{arch::global_asm, fmt};

//use fdt::{node::FdtNode, Fdt};
use rille::{
    //addr::VirtualConst,
    capability::{
        paging::{
            BasePage,
            Page,
            //PageTable, PageTableFlags
        },
        //Allocator,
        Captr,
        RemoteCaptr,
        Thread,
    },
    init::{BootInfo, InitCapabilities},
    syscalls::{
        self,
        //ecall1, SyscallNumber
    },
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

// fn print_node(node: FdtNode<'_, '_>, n_spaces: usize) {
//     (0..n_spaces).for_each(|_| print!(" "));
//     println!("{}/", node.name);

//     for property in node.properties() {
//         (0..n_spaces + 4).for_each(|_| print!(" "));

//         if let Some(v) = property.as_usize() {
//             println!("{}={}", property.name, v);
//         } else if let Some(v) = property.as_str() {
//             if v.is_ascii() && v.chars().all(|x| !x.is_ascii_control()) {
//                 println!("{}={:?}", property.name, v);
//             } else {
//                 println!("{}=...", property.name);
//             }
//         } else {
//             println!("{}=...", property.name);
//         }
//     }

//     for child in node.children() {
//         print_node(child, n_spaces + 4);
//     }
// }

extern "C" fn thread_entry(_thread: Captr<Thread>) -> ! {
    let _caps = unsafe { InitCapabilities::new() };
    //let thread = unsafe { Captr::from_raw_unchecked(thread) };
    loop {
        print!("\rthread 2!");
    }
    // thread.suspend().unwrap();

    // println!("resumed???");

    // thread.suspend().unwrap();
}

static THREAD_STACK: &[u8; 1024 * 1024] = &[0; 1024 * 1024];

#[no_mangle]
extern "C" fn entry(init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };
    let _init_info = unsafe { &*init_info };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    let _pg: Captr<Page<BasePage>> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65535) }, ())
        .unwrap();

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

    // pg.map(
    //     caps.pgtbl,
    //     VirtualConst::from_usize(0xDEAD0000),
    //     PageTableFlags::RW,
    // )
    // .unwrap();

    // unsafe { core::ptr::write_volatile(0xDEAD0000 as *mut u64, 0xC0DED00D) };

    // for i in 1..=6 {
    //     println!(
    //         "{i}: {:?}",
    //         syscalls::debug::debug_cap_identify(caps.captbl.into_raw(), i).unwrap()
    //     );
    // }

    // println!("{:#?}", init_info);

    // let fdt = unsafe { Fdt::<'static>::from_ptr(init_info.fdt_ptr) }.unwrap();
    // print_node(fdt.find_node("/").unwrap(), 0);

    unsafe { thread.resume().unwrap() }

    // thread.resume().unwrap();

    loop {
        print!("\rthread 1!");
    }

    // unsafe {
    //     ecall1(SyscallNumber::ThreadSuspend, caps.thread as u64).unwrap();
    // }

    // unreachable!();
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
