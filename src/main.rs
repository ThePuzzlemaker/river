#![feature(
    allocator_api,
    cell_update,
    new_uninit,
    alloc_error_handler,
    panic_info_message,
    maybe_uninit_slice,
    int_roundings,
    ptr_as_uninit,
    once_cell,
    core_intrinsics,
    slice_ptr_get
)]
#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn, clippy::semicolon_if_nothing_returned)]
#![warn(clippy::undocumented_unsafe_blocks, clippy::pedantic)]
#![allow(
    clippy::inline_always,
    clippy::must_use_candidate,
    clippy::cast_possible_truncation,
    clippy::module_name_repetitions,
    clippy::cast_ptr_alignment,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::fn_to_numeric_cast
)]

extern crate alloc;

pub mod addr;
pub mod asm;
pub mod boot;
pub mod hart_local;
pub mod kalloc;
pub mod once_cell;
pub mod paging;
pub mod plic;
pub mod proc;
pub mod spin;
pub mod symbol;
pub mod trampoline;
pub mod trap;
pub mod uart;
pub mod units;
pub mod util;

// Make sure the entry point is linked in
extern "C" {
    pub fn start() -> !;
}

use core::{
    alloc::Layout,
    arch::{asm, global_asm},
    panic::PanicInfo,
};

use addr::{Physical, Virtual};
use alloc::string::String;
use asm::hartid;
use fdt::Fdt;
use paging::{root_page_table, PageTableFlags};
use uart::UART;

use crate::{
    addr::{DirectMapped, Kernel, PhysicalMut, VirtualConst},
    hart_local::LOCAL_HART,
    kalloc::linked_list::LinkedListAlloc,
    plic::PLIC,
    proc::{Proc, ProcState, Scheduler},
    symbol::fn_user_code_woo,
    trampoline::Trapframe,
    trap::{Irqs, IRQS},
    units::StorageUnits,
};

#[global_allocator]
static HEAP_ALLOCATOR: LinkedListAlloc = LinkedListAlloc::new();

// It's unlikely the kernel itself is 64GiB in size, so we use this space.
pub const KHEAP_VMEM_OFFSET: usize = 0xFFFF_FFE0_0000_0000;
pub const KHEAP_VMEM_SIZE: usize = 64 * units::GIB;

global_asm!(
    "
.pushsection .user_code,\"ax\",@progbits
.type user_code_woo,@function
.global user_code_woo
user_code_woo:
    // syscall num = 0
    li a0, 0

    // str ptr = hello
    lla a1, hello

    // str len = hello_end - hello
    lla t0, hello_end
    lla t1, hello
    sub a2, t0, t1

    ecall
user_code_woo.loop:
    j user_code_woo.loop
hello:
    .byte 0x48,0x65,0x6c,0x6c,0x6f,0x2c,0x20,0x77,0x6f,0x72,0x6c,0x64,0x21,0x0a
hello_end:







.type user_code_woo2,@function
.global user_code_woo2
user_code_woo2:
    li a0, 64
    ecall
user_code_woo2.loop:
    j user_code_woo2
.popsection
"
);

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    println!("[info] initialized paging");

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
        let trampoline_virt =
            VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());
        let trampoline_phys = trampoline_virt.into_phys();
        pgtbl.map(
            trampoline_phys.into_identity(),
            Virtual::from_usize(usize::MAX - 4.kib() + 1),
            PageTableFlags::VAD | PageTableFlags::RX,
        );
    }
    println!("mapped 64GiB of virtual heap");
    // SAFETY: We guarantee that this virtual address is well-aligned and unaliased.
    unsafe { HEAP_ALLOCATOR.init(Virtual::from_usize(KHEAP_VMEM_OFFSET)) };

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(e) => panic!("error loading fdt: {}", e),
    };
    println!(
        "fdt @ {:#p} -> {:#x}",
        fdt_ptr,
        fdt_ptr as usize + fdt.total_size()
    );

    let plic = fdt
        .find_compatible(&["riscv,plic0"])
        .expect("Could not find a compatible PLIC");
    let plic_base_phys = PhysicalMut::<u8, DirectMapped>::from_ptr(
        plic.reg().unwrap().next().unwrap().starting_address as *mut _,
    );
    let plic_base = plic_base_phys.into_virt();
    // SAFETY: Our caller guarantees that the PLIC's address in the FDT is valid.
    unsafe { PLIC.init(plic_base.into_ptr_mut()) }
    let uart = fdt.chosen().stdout().unwrap();
    let uart_irq = uart.interrupts().unwrap().next().unwrap() as u32;
    PLIC.set_priority(uart_irq, 1);
    PLIC.hart_senable(uart_irq);
    PLIC.hart_set_spriority(0);
    // SAFETY: The trap vector address is valid.
    unsafe { asm::write_stvec(trap::kernel_trapvec as *const u8) }

    IRQS.get_or_init(|| Irqs { uart: uart_irq });

    asm::software_intr_on();
    asm::timer_intr_on();
    asm::external_intr_on();

    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

    let mut proc = Proc::new(String::from("user_mode_woo"));
    #[allow(clippy::undocumented_unsafe_blocks)]
    {
        let private = proc.private_mut();
        private.mem_size = 4.kib();
        let trapframe = unsafe { &mut *private.trapframe };
        let mut trapframe = trapframe.write(Trapframe::default());
        trapframe.user_epc = 0;
        trapframe.sp = 4.kib();
        private.pgtbl.map(
            VirtualConst::<u8, Kernel>::from_usize(fn_user_code_woo().into_usize())
                .into_phys()
                .into_identity(),
            VirtualConst::from_usize(0),
            PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RX,
        );
        proc.set_state(ProcState::Runnable);
    };
    // let mut proc2 = Proc::new(String::from("user_mode_woo2"));
    // #[allow(clippy::undocumented_unsafe_blocks)]
    // {
    //     let private = proc2.private_mut();
    //     private.mem_size = 4.kib();
    //     let trapframe = unsafe { &mut *private.trapframe };
    //     let mut trapframe = trapframe.write(Trapframe::default());
    //     trapframe.user_epc = 0x0c;
    //     trapframe.sp = 4.kib();
    //     private.pgtbl.map(
    //         VirtualConst::<u8, Kernel>::from_usize(fn_user_code_woo().into_usize())
    //             .into_phys()
    //             .into_identity(),
    //         VirtualConst::from_usize(0),
    //         PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RX,
    //     );
    //     proc2.set_state(ProcState::Runnable);
    // }

    Scheduler::init();
    Scheduler::enqueue(proc);
    //Scheduler::enqueue(proc2);

    LOCAL_HART.with(|hart| {
        //hart.push_off();
        println!(
            "=== river v0.0.-Inf ===\nboot hart id: {:?}\nhello world from kmain!",
            hart.hartid.get()
        );
        // panic!("{:#?}", asm::get_pagetable());

        // for hart in fdt
        //     .cpus()
        //     .filter(|cpu| cpu.ids().first() != hart.hartid.get() as usize)
        // {
        //     let id = hart.ids().first();
        //     println!("found FDT node for hart {id}");
        //     // Allocate a 4 MiB stack
        //     let sp_phys = {
        //         let mut pma = PMAlloc::get();
        //         pma.allocate(kalloc::phys::what_order(4.mib()))
        //             .expect("Could not allocate stack for hart")
        //     };

        //     let boot_data = Box::into_raw(Box::new(HartBootData {
        //         // We're both in the kernel--this hart will use the same SATP as us.
        //         raw_satp: get_satp(),
        //         sp_phys,
        //         fdt_ptr_virt: VirtualConst::<_, DirectMapped>::from_ptr(fdt_ptr),
        //     }));
        //     let boot_data_virt = VirtualConst::<_, Identity>::from_ptr(boot_data);
        //     let boot_data_phys = root_page_table()
        //         .lock()
        //         .walk(boot_data_virt.cast())
        //         .into_usize();

        //     let start_hart = boot::start_hart as *const u8;
        //     let start_hart_addr = VirtualConst::<_, Kernel>::from_ptr(start_hart)
        //         .into_phys()
        //         .into_usize();

        //     sbi::hsm::hart_start(id, start_hart_addr, boot_data_phys)
        //         .expect("Could not start hart");
        // }
        // hart.pop_off();

        let timebase_freq = fdt
            .cpus()
            .find(|cpu| cpu.ids().first() == hart.hartid.get() as usize)
            .expect("Could not find boot hart in FDT")
            .timebase_frequency() as u64;

        // About 1/10 sec.
        hart.timer_interval.set(timebase_freq / 10);
        let interval = hart.timer_interval.get();
        sbi::timer::set_timer(asm::read_time() + interval).unwrap();
    });

    // SAFETY: The scheduler is only started once on the main hart.
    unsafe { Scheduler::start() }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain_hart"]
extern "C" fn kmain_hart(fdt_ptr: *const u8) -> ! {
    println!("[{0}] [info] hart {0} starting", hartid());

    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(e) => panic!("error loading fdt: {e}"),
    };

    let uart = fdt.chosen().stdout().unwrap();
    let uart_irq = uart.interrupts().unwrap().next().unwrap() as u32;
    PLIC.set_priority(uart_irq, 1);
    PLIC.hart_senable(uart_irq);
    PLIC.hart_set_spriority(0);
    // SAFETY: The address provided is valid.
    unsafe { asm::write_stvec(trap::kernel_trapvec as *const u8) };

    asm::software_intr_on();
    asm::timer_intr_on();
    asm::external_intr_on();

    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

    loop {
        asm::nop();
    }
}

#[panic_handler]
pub fn panic_handler(panic_info: &PanicInfo) -> ! {
    if paging::enabled() {
        // Paging is set up, we can use dynamic dispatch.
        if let Some(msg) = panic_info.message() {
            println!("panic occurred: {}", msg);
            if let Some(location) = panic_info.location() {
                println!(
                    "  at {}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                );
            }
            /*let bt = Backtrace::<16>::capture();
            println!("  Backtrace:");
            for frame in bt.frames {
                println!("    - {:#x}", frame);
            }
            if bt.frames_omitted {
                println!("    - ... <omitted>");
            }*/
        } else {
            println!("panic occurred (reason unknown).\n");
            if let Some(location) = panic_info.location() {
                println!(
                    "  at {}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                );
            }
        }
    } else {
        UART.lock()
            .print_str_sync("panic occurred (reason unavailable).\n");
    }
    loop {
        asm::nop();
    }
}

#[alloc_error_handler]
fn alloc_error_handler(_: Layout) -> ! {
    panic!("out of memory")
}
