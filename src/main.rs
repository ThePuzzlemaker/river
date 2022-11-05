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
pub mod plic;
pub mod spin;
pub mod symbol;
pub mod trap;
pub mod uart;
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

use crate::{
    addr::{DirectMapped, PhysicalMut, KERNEL_PHYS_OFFSET},
    hart_local::LOCAL_HART,
    kalloc::linked_list::LinkedListAlloc,
    plic::PLIC,
    trap::{Irqs, IRQS},
    uart::UART,
    units::StorageUnits,
};
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
        while (unsafe { serial.add(5).read_volatile() } & (1 << 5)) == 0 {
            hint::spin_loop();
        }
        unsafe { serial.write_volatile(*b) };
    }
}

fn get_serial() -> *mut u8 {
    SERIAL.load(Ordering::Relaxed)
}

// #[no_mangle]
// #[link_section = ".init.trapvec"]
// pub extern "C" fn trap() {
//     loop {
//         nop()
//     }
// }

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

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(_e) => panic!("error loading fdt"),
    };
    println_hacky!("fdt @ {:#p} -> {:#p}", fdt_ptr, unsafe {
        fdt_ptr.add(fdt.total_size())
    });

    let plic = fdt
        .find_compatible(&["riscv,plic0"])
        .expect("Could not find a compatible PLIC");
    let plic_base_phys = PhysicalMut::<u8, DirectMapped>::from_ptr(
        plic.reg().unwrap().next().unwrap().starting_address as *mut _,
    );
    let plic_base = plic_base_phys.into_virt();
    unsafe { PLIC.init(plic_base.into_ptr_mut()) }
    let uart = fdt.chosen().stdout().unwrap();
    let uart_irq = uart.interrupts().unwrap().next().unwrap() as u32;
    unsafe { PLIC.set_priority(uart_irq, 1) }
    unsafe { PLIC.hart_senable(uart_irq) }
    unsafe { PLIC.hart_set_spriority(0) }
    unsafe { asm!("csrw stvec, {}", in(reg) trap::kernel_trapvec) }

    unsafe { UART.lock().init(get_serial()) }
    IRQS.get_or_init(|| Irqs { uart: uart_irq });

    asm::software_intr_on();
    asm::external_intr_on();

    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

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
