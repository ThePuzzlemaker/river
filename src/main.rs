#![feature(
    allocator_api,
    cell_update,
    new_uninit,
    alloc_error_handler,
    panic_info_message,
    maybe_uninit_slice,
    int_log,
    int_roundings,
    ptr_as_uninit,
    once_cell,
    core_intrinsics,
    slice_ptr_get
)]
#![no_std]
#![no_main]

extern crate alloc;

pub mod addr;
pub mod asm;
pub mod boot;
pub mod hart_local;
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

use addr::{Physical, Virtual};
use alloc::boxed::Box;
use asm::hartid;
use fdt::Fdt;
use paging::{root_page_table, PageTableFlags};

use crate::{kalloc::linked_list::LinkedListAlloc, units::StorageUnits};
// use kalloc::slab::Slab;

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
static HEAP_ALLOCATOR: LinkedListAlloc = LinkedListAlloc::new();

#[macro_export]
macro_rules! println_hacky {
    ($fmt:literal$(, $($tt:tt)*)?) => {{
        ::core::fmt::write(&mut $crate::Serial, ::core::format_args!($fmt$(, $($tt)*)?)).expect("i/o");
        $crate::print("\n");
    }}
}

// It's unlikely the kernel itself is 64GiB in size, so we use this space.
pub const KHEAP_VMEM_OFFSET: usize = 0xFFFFFFE000000000;
pub const KHEAP_VMEM_SIZE: usize = 64 * units::GIB;

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
pub extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    unsafe { asm!("csrw stvec, {}", in(reg) trap) }
    print("[info] initialized paging\n");

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
    //         PageTableFlags::RW | PageTableFlags::VAD,
    //     );
    // }

    {
        let mut pgtbl = root_page_table().lock();
        for offset in 0..64 {
            // This mapping is invalid, so the physical addr is irrelevant.
            pgtbl.map_gib(
                Physical::from_usize(0),
                Virtual::from_usize(KHEAP_VMEM_OFFSET + offset.gib()),
                PageTableFlags::empty(),
            );
        }
    }
    println_hacky!("mapped 64GiB of virtual heap");
    unsafe { HEAP_ALLOCATOR.init(Virtual::from_usize(KHEAP_VMEM_OFFSET)) };

    // let page = {
    //     let mut pma = PMAlloc::get();
    //     pma.allocate(0).expect("oom")
    // };
    // {
    //     let mut pgtbl = root_page_table().lock();
    //     pgtbl.map(
    //         page.into_identity().into_const(),
    //         Virtual::from_usize(KHEAP_VMEM_OFFSET),
    //         PageTableFlags::VAD | PageTableFlags::READ | PageTableFlags::WRITE,
    //     );
    // }
    unsafe {
        let mut v1 = Box::<[u64], _>::new_uninit_slice(4096);
        for i in 0..4096 {
            v1[i].as_mut_ptr().write(i as u64);
        }
        let v1 = v1.assume_init();
        println_hacky!("{:?}", v1);
        // let v1 = Box::into_raw(Box::new_in(0xdeadbeefc0ded00d_u64, &kalloc));
        // let v2 = Box::into_raw(Box::new_in(0xc0ffee00_u32, &kalloc));
        // println_hacky!("v1@{:#p}={:#x}, v2@{:#p}={:#x}", v1, *v1, v2, *v2);
        // drop(Box::from_raw_in(v1, &kalloc));
        // drop(Box::from_raw_in(v2, &kalloc));
        // let v3 = Box::into_raw(Box::new_in(0xf00dd00d_u32, &kalloc));
        // let v4 = Box::into_raw(Box::new_in(
        //     0x102030405060708090a0b0c0d0e0f0ff_u128,
        //     &kalloc,
        // ));
        // println_hacky!("v1@{:#p}={:#x}, v2@{:#p}={:#x}", v1, *v1, v2, *v2);
    }

    // let slab = Slab::new(Layout::new::<u64>());
    // println_hacky!("slab = {:#p}", slab.as_ptr());
    // let val = unsafe { &*slab.as_ptr() };
    // println_hacky!("val = {:?}", val);
    // let mut loop_next = val.free_list;
    // while let Some(next) = loop_next {
    //     println_hacky!("ptr = {:#p}", next);
    //     loop_next = unsafe { &*next.as_ptr() }.next;
    // }

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let _fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(_e) => panic!("error loading fdt"),
    };

    fmt::write(
        &mut Serial,
        format_args!(
            "=== river v0.0.-Inf ===\nboot hart id: {:?}\nhello world from kmain!\n",
            hartid()
        ),
    )
    .expect("i/o");
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
