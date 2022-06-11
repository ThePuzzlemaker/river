#![feature(
    allocator_api,
    cell_update,
    new_uninit,
    alloc_error_handler,
    panic_info_message,
    maybe_uninit_slice,
    int_log,
    int_roundings
)]
#![no_std]
#![no_main]

extern crate alloc;

pub mod addr;
pub mod asm;
pub mod boot;
pub mod kalloc;
pub mod once_cell;
pub mod paging;
pub mod spin;
pub mod symbol;
pub mod units;
pub mod util;

// Make sure the entry point is linked in
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

#[global_allocator]
static HEAP_ALLOCATOR: OomAlloc = OomAlloc;

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
pub extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    unsafe { asm!("csrw stvec, {}", in(reg) trap) }

    // // Allocate a 64-MiB heap.
    // let heap_base: VirtualMut<u8, Identity> = VirtualMut::from_usize(0x0010_0000);
    // let mut pgtbl = root_page_table().lock();
    // for offset in (0..64.mib()).step_by(4096) {
    //     let curr_page = {
    //         let mut pma = PMAlloc::get();
    //         pma.allocate().expect("oom")
    //     };
    //     pgtbl.map(
    //         curr_page
    //             .into_physical()
    //             .into_identity()
    //             .cast()
    //             .into_const(),
    //         heap_base.add(offset).into_const(),
    //         PageTableFlags::RW | PageTableFlags::VALID,
    //     );
    // }

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let _fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(_e) => panic!("error loading fdt"),
    };
    let resv = _fdt.find_node("/reserved-memory").unwrap();
    for child in resv.children() {
        fmt::write(
            &mut Serial,
            format_args!(
                "name={:?}, reg={:?}\n",
                child.name,
                child.reg().unwrap().next().unwrap()
            ),
        )
        .unwrap();
    }

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
            fmt::write(&mut Serial, format_args!("panic occurred: {}\n", msg)).expect("write");
            if let Some(location) = panic_info.location() {
                fmt::write(
                    &mut Serial,
                    format_args!(
                        "  at {}:{}:{}\n",
                        location.file(),
                        location.line(),
                        location.column()
                    ),
                )
                .expect("write");
            }
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

struct OomAlloc;
unsafe impl GlobalAlloc for OomAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        panic!("OomAlloc::allocate")
    }

    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {
        panic!("OomAlloc::deallocate")
    }
}
