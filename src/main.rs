#![feature(
    allocator_api,
    cell_update,
    new_uninit,
    alloc_error_handler,
    panic_info_message,
    maybe_uninit_slice
)]
#![no_std]
#![no_main]

extern crate alloc;

pub mod addr;
pub mod asm;
pub mod boot;
pub mod paging;
pub mod phys;
pub mod spin;
pub mod symbol;
pub mod units;
pub mod util;

extern "C" {
    pub fn start() -> !;
}

use core::{
    alloc::{GlobalAlloc, Layout},
    arch::asm,
    fmt, hint,
    panic::PanicInfo,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use fdt::Fdt;

static SERIAL: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

// use uart_16550::MmioSerialPort;

// pub fn serial() -> MmioSerialPort {
//     unsafe { MmioSerialPort::new(SERIAL_PORT_BASE_ADDRESS) }
// }

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn print(s: &str) {
    let serial = get_serial();
    for b in s.as_bytes() {
        while (unsafe { ptr::read_volatile(serial.add(5)) } & (1 << 5)) == 0 {
            hint::spin_loop();
        }
        unsafe { ptr::write_volatile(serial, *b) };
    }
}

fn get_serial() -> *mut u8 {
    SERIAL.load(Ordering::Relaxed)
}

#[no_mangle]
#[link_section = ".init.trapvec"]
pub extern "C" fn trap() {
    loop {
        nop()
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
pub extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    unsafe { asm!("csrw stvec, {}", in(reg) trap) }

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let _fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(_e) => panic!("error loading fdt"),
    };

    print("Hello, world from kmain!\n");
    // panic!("{:#?}", asm::get_pagetable());

    loop {
        nop()
    }
}

#[inline(always)]
pub fn nop() {
    unsafe { asm!("nop") }
}

pub struct Serial;
impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print(s);
        Ok(())
    }
}

#[panic_handler]
pub fn panic_handler(panic_info: &PanicInfo) -> ! {
    if paging::enabled() {
        // Paging is set up, we can use dynamic dispatch.
        if let Some(msg) = panic_info.message() {
            print("panic occurred: ");
            fmt::write(&mut Serial, *msg).unwrap();
            print("\n");
        } else if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            print("panic occurred: ");
            print(s);
            print("\n");
        } else {
            print("panic occurred (reason unknown).\n");
        }
    } else {
        print("panic occurred (reason unavailable).\n");
    }
    loop {
        nop()
    }
}

#[alloc_error_handler]
pub fn alloc_error_handler(_: Layout) -> ! {
    panic!("out of memory")
}

struct OomAllocator;

#[global_allocator]
static GLOBAL_ALLOC_TMP: OomAllocator = OomAllocator;

unsafe impl GlobalAlloc for OomAllocator {
    unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
        panic!("no global allocator")
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        panic!("no global allocator")
    }
}
