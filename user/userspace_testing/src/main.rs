#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{
    arch::{asm, global_asm},
    fmt,
    num::NonZeroU64,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use rille::{
    addr::VirtualConst,
    capability::{
        paging::{BasePage, MegaPage, Page, PageTableFlags, PagingLevel},
        CapRights, Captr, Notification, RemoteCaptr, Thread,
    },
    init::{BootInfo, InitCapabilities},
    syscalls::{self, ecall1, ecall2, ecall4, ecall6, SyscallNumber},
};
use syncutils::{mutex::Mutex, once_cell::OnceCell};

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
    unsafe {
        asm!("li tp, 1", options(nomem));
    }

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
    loop {
        let mut lock = SHARED2.expect("oops").lock();
        let n = *lock;

        if n >= 50_000 {
            break;
        }
        *lock += 1;
        print!("\rthread 2: {}       {:?}      ", n, time());
    }
    // out_notif.signal().unwrap();

    // while in_notif.wait().is_ok() {
    //     let n = SHARED.fetch_add(1, Ordering::Relaxed);

    //     //print!("\rpong!: {}       {:?}     ", n, time());

    //     if n == 50_000 {
    //         break;
    //     }
    //     out_notif.signal().unwrap()
    // }

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

extern "C" fn thread2_entry(_thread: Captr<Thread>, init_info: *const BootInfo) -> ! {
    // unsafe {
    //     asm!("li tp, 2", options(nomem));
    // }
    let init_info = unsafe { &*init_info };
    let init_caps = unsafe { InitCapabilities::new() };
    let notif: Captr<Notification> = init_caps
        .allocator
        .allocate(
            RemoteCaptr::local(init_caps.captbl),
            init_info.free_slots.lo.add(1),
            (),
        )
        .unwrap();
    unsafe {
        ecall6(
            SyscallNumber::Grant,
            init_caps.captbl.into_raw() as u64,
            notif.into_raw() as u64,
            init_caps.captbl.into_raw() as u64,
            notif.into_raw() as u64,
            CapRights::all().bits(),
            0xDEADBEEF,
        )
        .unwrap();
    }

    // unsafe {
    //     ecall4(
    //         SyscallNumber::IntrPoolGet,
    //         init_caps.intr_pool.into_raw() as u64,
    //         init_caps.captbl.into_raw() as u64,
    //         init_info.free_slots.lo.add(2).into_raw() as u64,
    //         0x0a,
    //     )
    //     .unwrap();
    // }
    let intr_handler = init_info.free_slots.lo.add(2);

    // unsafe {
    //     ecall2(
    //         SyscallNumber::IntrHandlerBind,
    //         intr_handler.into_raw() as u64,
    //         notif.into_raw() as u64,
    //     )
    //     .unwrap();
    // }

    // syscalls::debug::debug_dump_root();

    while notif.wait().unwrap().is_some() {
        unsafe {
            ecall1(
                SyscallNumber::IntrHandlerAck,
                intr_handler.into_raw() as u64,
            )
            .unwrap();
        }
        //        print!("\rexternal intr!");
        // syscalls::debug::debug_cap_slot(init_caps.captbl.into_raw(), notif.into_raw()).unwrap();
    }

    loop {
        unsafe {
            core::arch::asm!("nop");
        }
    }
}

static SHARED: AtomicU64 = AtomicU64::new(0);
static SHARED2: OnceCell<Mutex<u64>> = OnceCell::new();

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
    let init_info = unsafe { &*init_info };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    let thread: Captr<Thread> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65534) }, ())
        .unwrap();
    let thread2: Captr<Thread> = caps
        .allocator
        .allocate(root_captbl, init_info.free_slots.lo, ())
        .unwrap();

    let pg: Captr<Page<MegaPage>> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65000) }, ())
        .unwrap();
    let pg2: Captr<Page<MegaPage>> = caps
        .allocator
        .allocate(root_captbl, unsafe { Captr::from_raw_unchecked(65001) }, ())
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

    pg.map(
        caps.pgtbl,
        VirtualConst::from_usize(0x8200000),
        PageTableFlags::RW,
    )
    .unwrap();
    pg2.map(
        caps.pgtbl,
        VirtualConst::from_usize(0x8600000),
        PageTableFlags::RW,
    )
    .unwrap();
    init_info
        .dev_pages
        .lo
        .map(
            caps.pgtbl,
            VirtualConst::from_usize(0x80000000),
            PageTableFlags::RW,
        )
        .unwrap();

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
    unsafe { thread2.configure(Captr::null(), Captr::null()).unwrap() };
    unsafe {
        thread
            .write_registers(&rille::capability::UserRegisters {
                pc: thread_entry as usize as u64,
                sp: 0x8200000 + (1 << MegaPage::PAGE_SIZE_LOG2),
                a0: thread.into_raw() as u64,
                a1: out_notif.into_raw() as u64,
                a2: in_notif.into_raw() as u64,
                ..Default::default()
            })
            .unwrap()
    };
    unsafe {
        thread2
            .write_registers(&rille::capability::UserRegisters {
                pc: thread2_entry as usize as u64,
                sp: 0x8600000 + (1 << MegaPage::PAGE_SIZE_LOG2),
                a0: thread2.into_raw() as u64,
                a1: init_info as *const _ as u64,
                ..Default::default()
            })
            .unwrap()
    };

    //unsafe { thread.resume().unwrap() }

    unsafe { thread2.resume().unwrap() }

    unsafe { thread.resume().unwrap() }

    SHARED2.get_or_init(|| Mutex::new(0, in_notif));

    loop {
        let mut lock = SHARED2.expect("oops").lock();
        let n = *lock;

        if n >= 50_000 {
            println!("");
            break;
        }
        *lock += 1;
        print!("\rthread 1: {}       {:?}      ", n, time());
    }

    // while in_notif.wait().is_ok() {
    //     let n = SHARED.fetch_add(1, Ordering::Relaxed);

    //     //        print!("\rping!: {}       {:?}     ", n, time());
    //     if n == 50_000 {
    //         println!("\ndone! {:#?}", time());
    //         break;
    //     }
    //     out_notif.signal().unwrap();
    // }

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
        print(s);
        Ok(())
    }
}

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
