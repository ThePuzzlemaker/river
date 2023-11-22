#![no_std]
#![no_main]
#![feature(panic_info_message)]

use core::{
    arch::{asm, global_asm},
    fmt,
};

use rille::{
    addr::VirtualConst,
    capability::{
        paging::{BasePage, Page, PageTable, PageTableFlags},
        Captr, RemoteCaptr,
    },
    init::{BootInfo, InitCapabilities},
    syscalls,
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
.pushsection .text
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

#[no_mangle]
extern "C" fn entry(_init_info: *const BootInfo) -> ! {
    let caps = unsafe { InitCapabilities::new() };

    let _root_captbl = RemoteCaptr::local(caps.captbl);

    let pg: Captr<Page<BasePage>> = unsafe { Captr::from_raw_unchecked(2) };
    let pgtbl: Captr<PageTable> = unsafe { Captr::from_raw_unchecked(3) };

    // syscalls::debug::debug_dump_root();

    pg.map(
        pgtbl,
        VirtualConst::from_usize(0xDEAD0000),
        PageTableFlags::RW,
    )
    .unwrap();

    // unsafe { core::ptr::write_volatile(0xDEAD0000 as *mut u64, 0xC0DED00D) };

    // println!("{:#x?}", unsafe {
    //     core::ptr::read_volatile(0xDEAD0000 as *mut u64)
    // });

    // syscalls::debug::debug_dump_root();
    for i in 1..=5 {
        println!(
            "{i}: {:?}",
            syscalls::debug::debug_cap_identify(caps.captbl.into_raw(), i).unwrap()
        );
    }

    // let slot_2 = root_captbl
    //     .copy_deep(root_captbl.local_index(), root_captbl, unsafe {
    //         Captr::from_raw_unchecked(2)
    //     })
    //     .unwrap();

    // let _slot_3 = root_captbl
    //     .copy_deep(root_captbl.local_index(), root_captbl, unsafe {
    //         Captr::from_raw_unchecked(3)
    //     })
    //     .unwrap();

    // let _slot_4 = root_captbl
    //     .copy_deep(slot_2, root_captbl, unsafe { Captr::from_raw_unchecked(5) })
    //     .unwrap();

    // let mut total_order = ["root", "2", "3", "2a"];

    // let mut rand = SmallRng::seed_from_u64(0xDEADBEEF);

    // for i in 0..10_000 {
    //     let n1 = (rand.next_u64() % 3 + 1) as usize;
    //     let n2 = (rand.next_u64() % 3 + 1) as usize;
    //     print!("\rRound {}: Swapping {} with {}", i + 1, n1, n2);
    //     total_order.swap(n1 - 1, n2 - 1);
    //     RemoteCaptr::remote(root_captbl.local_index(), unsafe {
    //         Captr::<Captbl>::from_raw_unchecked(n1)
    //     })
    //     .swap(RemoteCaptr::remote(root_captbl.local_index(), unsafe {
    //         Captr::<Captbl>::from_raw_unchecked(n2)
    //     }))
    //     .unwrap();
    // }

    // println!("\nTotal order: {:?}", total_order);
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
