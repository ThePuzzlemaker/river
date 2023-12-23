#![no_std]
#![no_main]
#![feature(maybe_uninit_slice)]

use core::{arch::global_asm, mem::MaybeUninit};

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use rille::{
    capability::{
        paging::{BasePage, MegaPage, Page, PageTable},
        Capability, Captr, CaptrOps, Endpoint, Job, Notification, Thread,
    },
    syscalls,
};
use rille_user::{malloc::LinkedListAlloc, sync::once_cell::OnceCell};

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

#[global_allocator]
static GLOBAL_ALLOC: OnceCell<LinkedListAlloc> = OnceCell::new();

#[no_mangle]
unsafe extern "C" fn entry(ep: Endpoint, n_pages: usize) -> ! {
    let mut job = Captr::null();
    let mut thread = Captr::null();
    let mut stack_pg1 = Captr::null();
    let mut stack_pg2 = Captr::null();
    let mut ipc_buf = Captr::null();
    let mut pgtbl = Captr::null();

    ep.recv_with_regs(None, Some(&mut job)).unwrap();
    ep.recv_with_regs(None, Some(&mut thread)).unwrap();
    ep.recv_with_regs(None, Some(&mut stack_pg1)).unwrap();
    ep.recv_with_regs(None, Some(&mut stack_pg2)).unwrap();
    ep.recv_with_regs(None, Some(&mut ipc_buf)).unwrap();
    ep.recv_with_regs(None, Some(&mut pgtbl)).unwrap();

    let job = Job::from_captr(job);
    let thread = Thread::from_captr(thread);
    let stack_pg1 = Page::<MegaPage>::from_captr(stack_pg1);
    let stack_pg2 = Page::<MegaPage>::from_captr(stack_pg2);
    let ipc_buf = Page::<BasePage>::from_captr(ipc_buf);
    let pgtbl = PageTable::from_captr(pgtbl);

    // SAFETY: The provided base address is valid and can be mapped.
    unsafe {
        GLOBAL_ALLOC
            .get_or_init(|| LinkedListAlloc::new(Notification::create().unwrap(), pgtbl))
            .init(0xF000_0000 as *mut u8);
    }

    let mut init_thread = Captr::null();
    let mut root_job = Captr::null();

    ep.recv_with_regs(None, Some(&mut init_thread)).unwrap();
    ep.recv_with_regs(None, Some(&mut root_job)).unwrap();

    let init_thread = Thread::from_captr(init_thread);
    let root_job = Job::from_captr(root_job);

    let mut image_pages = vec![Captr::null(); n_pages];
    for pg in image_pages.iter_mut() {
        ep.recv_with_regs(None, Some(pg)).unwrap();
    }

    let mut sender = None;
    let hdr = ep.recv_with_regs(Some(&mut sender), None).unwrap();
    core::fmt::write(&mut DebugPrint, format_args!("{hdr:#x?} {sender:#x?}\n")).unwrap();

    syscalls::debug::debug_dump_root();

    // Kill and delete the init thread.
    init_thread.suspend().unwrap();
    init_thread.delete().unwrap();

    syscalls::debug::debug_dump_root();

    loop {
        core::arch::asm!("nop");
    }
}

#[panic_handler]
unsafe fn panic_handler(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::arch::asm!("nop")
    }
}

struct DebugPrint;

impl core::fmt::Write for DebugPrint {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        syscalls::debug::debug_print_string(s);
        Ok(())
    }
}
