#![no_std]
#![no_main]

use core::{arch::asm, fmt, panic::PanicInfo};

use rille::{
    capability::{Capability, Captbl, Captr, RemoteCaptr},
    syscalls::{ecall0, ecall2, SyscallNumber},
};

#[no_mangle]
unsafe fn _start() -> ! {
    let root_captbl: RemoteCaptr<Captbl> = RemoteCaptr::local(Captr::from_raw_unchecked(1));

    let new_captr = root_captbl
        .copy_deep(
            root_captbl.local_index(),
            root_captbl,
            Captr::from_raw_unchecked(2),
        )
        .unwrap();
    debug_print("captr 1:\n");
    debug_capslot(root_captbl);
    debug_print("captr 2:\n");
    debug_capslot(RemoteCaptr::local(new_captr));

    let captr3 = RemoteCaptr::remote(root_captbl.local_index(), new_captr)
        .copy_deep(
            root_captbl.local_index(),
            RemoteCaptr::remote(root_captbl.local_index(), new_captr),
            Captr::from_raw_unchecked(3),
        )
        .unwrap();
    debug_print("captr 3:\n");
    debug_capslot(RemoteCaptr::local(captr3));

    debug_print("final cap table: \n");
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

unsafe fn debug_capslot<C: Capability>(cap: RemoteCaptr<C>) {
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
