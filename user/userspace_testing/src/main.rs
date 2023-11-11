#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{arch::asm, fmt, panic::PanicInfo};

use rand::{rngs::SmallRng, RngCore, SeedableRng};
use rille::{
    addr::Vpn,
    capability::{
        paging::{BasePage, GigaPage, MegaPage, PageCaptr, PageTableFlags, PgTblCaptr},
        Capability,
        Captbl,
        Captr,
        RemoteCaptr, //Untyped,
    },
    init::{BootInfo, InitCapabilities},
    syscalls::{self, ecall0, ecall2, SyscallNumber},
};

extern crate panic_handler;

macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	syscalls::debug::debug_print_string("\n")
    }}
}

macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
    }}
}

#[no_mangle]
extern "C" fn _start(init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };

    let root_captbl = RemoteCaptr::local(caps.captbl);

    let slot_2 = root_captbl
        .copy_deep(root_captbl.local_index(), root_captbl, unsafe {
            Captr::from_raw_unchecked(2)
        })
        .unwrap();

    let _slot_3 = root_captbl
        .copy_deep(root_captbl.local_index(), root_captbl, unsafe {
            Captr::from_raw_unchecked(3)
        })
        .unwrap();

    let _slot_4 = root_captbl
        .copy_deep(slot_2, root_captbl, unsafe { Captr::from_raw_unchecked(4) })
        .unwrap();

    let mut total_order = ["root", "2", "3", "2a"];

    let mut rand = SmallRng::seed_from_u64(0xDEADBEEF);

    for i in 0..10_000 {
        let n1 = (rand.next_u64() % 3 + 1) as usize;
        let n2 = (rand.next_u64() % 3 + 1) as usize;
        print!("\rRound {}: Swapping {} with {}", i + 1, n1, n2);
        total_order.swap(n1 - 1, n2 - 1);
        RemoteCaptr::remote(root_captbl.local_index(), unsafe {
            Captr::<Captbl>::from_raw_unchecked(n1)
        })
        .swap(RemoteCaptr::remote(root_captbl.local_index(), unsafe {
            Captr::<Captbl>::from_raw_unchecked(n2)
        }))
        .unwrap();
    }

    println!("\nTotal order: {:?}", total_order);
    //    let untyped = unsafe { Captr::<Untyped>::from_raw_unchecked(4) };

    // let page: PageCaptr<BasePage> = untyped
    //     .retype(root_captbl, unsafe { Captr::from_raw_unchecked(2) }, ())
    //     .unwrap();

    // let pg_l0: PgTblCaptr<GigaPage> = untyped
    //     .retype(root_captbl, unsafe { Captr::from_raw_unchecked(5) }, ())
    //     .unwrap();
    // let pg_l1: PgTblCaptr<MegaPage> = untyped
    //     .retype(root_captbl, unsafe { Captr::from_raw_unchecked(6) }, ())
    //     .unwrap();
    // let pg_l2: PgTblCaptr<BasePage> = untyped
    //     .retype(root_captbl, unsafe { Captr::from_raw_unchecked(7) }, ())
    //     .unwrap();

    // page.map(pg_l2, Vpn::from(126u16), PageTableFlags::RW)
    //     .unwrap();
    // pg_l2
    //     .map(pg_l1, Vpn::from(126u16), PageTableFlags::RW)
    //     .unwrap();
    // pg_l1
    //     .map(pg_l0, Vpn::from(126u16), PageTableFlags::RW)
    //     .unwrap();

    syscalls::debug::debug_dump_root();

    loop {
        unsafe { asm!("pause") };
    }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
