#![no_std]
#![no_main]

use core::{arch::asm, fmt, panic::PanicInfo};

use rand::{rngs::SmallRng, RngCore, SeedableRng};
use rille::{
    capability::{Capability, Captbl, Captr, RemoteCaptr, Untyped},
    syscalls::{ecall0, ecall2, SyscallNumber},
};

macro_rules! println {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
	// SAFETY: .
	unsafe { debug_print("\n") }
    }}
}

macro_rules! print {
    ($($tt:tt)*) => {{
	core::fmt::write(&mut DebugPrint, format_args!($($tt)*)).unwrap();
    }}
}

#[no_mangle]
unsafe fn _start() -> ! {
    let root_captbl: RemoteCaptr<Captbl> = RemoteCaptr::local(Captr::from_raw_unchecked(1));
    let _untyped: Captr<Untyped> = Captr::from_raw_unchecked(4);

    let slot_2 = root_captbl
        .copy_deep(
            root_captbl.local_index(),
            root_captbl,
            Captr::from_raw_unchecked(2),
        )
        .unwrap();

    let _slot_3 = root_captbl
        .copy_deep(
            root_captbl.local_index(),
            root_captbl,
            Captr::from_raw_unchecked(3),
        )
        .unwrap();
    let _slot_5 = root_captbl
        .copy_deep(
            root_captbl.local_index(),
            root_captbl,
            Captr::from_raw_unchecked(5),
        )
        .unwrap();
    let _slot_6 = RemoteCaptr::local(slot_2)
        .copy_deep(slot_2, root_captbl, Captr::from_raw_unchecked(6))
        .unwrap();

    ecall0(SyscallNumber::DebugDumpRoot).unwrap();

    let mut total = [
        "captblRoot",
        "captbl2",
        "captbl3",
        "untyped",
        "captbl4",
        "captbl2a",
    ];

    let mut rng = SmallRng::seed_from_u64(0xFEEDC0DE_C0DED00D);
    for i in 0..100000 {
        let v1 = rng.next_u64() % 6 + 1;
        let v2 = rng.next_u64() % 6 + 1;
        print!("\rRound {}: Swapping {v1} and {v2}", i + 1);
        let v1: RemoteCaptr<Captbl> = RemoteCaptr::local(Captr::from_raw_unchecked(v1 as usize));
        let v2: RemoteCaptr<Captbl> = RemoteCaptr::local(Captr::from_raw_unchecked(v2 as usize));
        v1.swap(v2).unwrap();
        total.swap(
            v1.local_index().into_raw() - 1,
            v2.local_index().into_raw() - 1,
        )
        //ecall0(SyscallNumber::DebugDumpRoot).unwrap();
    }

    println!(
        "\nTotal modification order: {}, {}, {}, {}, {}, {}",
        total[0], total[1], total[2], total[3], total[4], total[5]
    );

    // let new_captbl = untyped
    //     .retype_many(
    //         RemoteCaptr::remote(root_captbl.local_index(), new_captr),
    //         Captr::from_raw_unchecked(5),
    //         1,
    //         CaptblSizeSpec { n_slots_log2: 6 },
    //     )
    //     .unwrap();

    // debug_print("captr 1:\n");
    // debug_capslot(root_captbl);
    // debug_print("captr 2:\n");
    // debug_capslot(RemoteCaptr::local(new_captr));

    // for new_captbl in new_captbl {
    //     let captr3 = RemoteCaptr::remote(root_captbl.local_index(), new_captr)
    //         .copy_deep(
    //             root_captbl.local_index(),
    //             RemoteCaptr::remote(root_captbl.local_index(), new_captbl),
    //             Captr::from_raw_unchecked(1),
    //         )
    //         .unwrap();
    //     debug_capslot(RemoteCaptr::local(new_captbl));
    // }
    // debug_print("captr 3:\n");
    // debug_capslot(RemoteCaptr::local(captr3));

    // debug_print("final cap table: \n");
    ecall0(SyscallNumber::DebugDumpRoot).unwrap();

    loop {
        asm!("nop");
    }
}

struct DebugPrint;

impl fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        unsafe { debug_print(s) }
        Ok(())
    }
}

unsafe fn debug_print(s: &str) {
    ecall2(SyscallNumber::DebugPrint, s.as_ptr() as u64, s.len() as u64).unwrap();
}

unsafe fn _debug_capslot<C: Capability>(cap: RemoteCaptr<C>) {
    ecall2(
        SyscallNumber::DebugCapSlot,
        cap.reftbl().into_raw() as u64,
        cap.local_index().into_raw() as u64,
    )
    .unwrap();
}

#[panic_handler]
unsafe fn panic(_panic: &PanicInfo<'_>) -> ! {
    debug_print("panic");
    loop {
        asm!("nop");
    }
}
