#![feature(
    allocator_api,
    cell_update,
    new_uninit,
    alloc_error_handler,
    panic_info_message,
    maybe_uninit_slice,
    int_roundings,
    ptr_as_uninit,
    core_intrinsics,
    slice_ptr_get,
    get_many_mut
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

pub mod asm;
pub mod boot;
pub mod capability;
pub mod elf;
pub mod hart_local;
pub mod kalloc;
pub mod paging;
pub mod plic;
pub mod proc;
pub mod symbol;
pub mod sync;
pub mod syslog;
pub mod trampoline;
pub mod trap;
pub mod uart;
pub mod util;

// Make sure the entry point is linked in
extern "C" {
    pub fn start() -> !;
}

use core::{
    alloc::{GlobalAlloc, Layout},
    arch::{asm, global_asm},
    mem::MaybeUninit,
    panic::PanicInfo,
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};

use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc};
use asm::hartid;
use fdt::Fdt;
use paging::{root_page_table, PageTableFlags};
use rille::{
    addr::{DirectMapped, Identity, Kernel, Physical, PhysicalMut, Virtual, VirtualConst},
    capability::paging::PageSize,
    units::StorageUnits,
};
use uart::UART;

use crate::{
    boot::HartBootData,
    capability::{captbl::Captbl, AllocatorSlot, EmptySlot, Page, PgTbl, Thread, ThreadState},
    hart_local::LOCAL_HART,
    kalloc::{linked_list::LinkedListAlloc, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    plic::PLIC,
    proc::{Scheduler, SchedulerInner},
    sync::{SpinMutex, SpinRwLock},
    trap::{Irqs, IRQS},
};

#[global_allocator]
static HEAP_ALLOCATOR: LinkedListAlloc = LinkedListAlloc::new();
//static HEAP_ALLOCATOR: NoAlloc = NoAlloc;

struct NoAlloc;

// SAFETY: We don't allocate, and it's safe to panic.
unsafe impl GlobalAlloc for NoAlloc {
    #[track_caller]
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        unimplemented!()
    }

    #[track_caller]
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {
        unimplemented!()
    }
}

// It's unlikely the kernel itself is 64GiB in size, so we use this space.
pub const KHEAP_VMEM_OFFSET: usize = 0xFFFF_FFE0_0000_0000;
pub const KHEAP_VMEM_SIZE: usize = 64 * rille::units::GIB;

global_asm!(
    "
.pushsection .user_code,\"ax\",@progbits
.type user_code_woo,@function
.global user_code_woo
user_code_woo:
    li s0, 0
user_code_woo.loop:
    li a0, 9

    lla a1, hello

    lla t0, hello_end
    lla t1, hello
    sub a2, t0, t1

    ecall

    //li a0, 1
    //ecall

    j user_code_woo.loop

//     //beq s0, x0, user_code_woo.loop
// user_code_woo.loop_spin:
//     //li s0, 1
//     j user_code_woo.loop_spin	

//.section .rodata
hello:
    .byte 0x48,0x61,0x69,0x69,0x69,0x20,0x77,0x6f,0x72,0x6c,0x64,0x21,0x21,0x0a
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

pub static INIT: &[u8] = include_bytes!(env!("CARGO_BUILD_INIT_PATH"));

static N_STARTED: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(e) => panic!("error loading fdt: {}", e),
    };

    LOCAL_HART.with(|hart| {
        let timebase_freq = fdt
            .cpus()
            .find(|cpu| cpu.ids().first() == hart.hartid.get() as usize)
            .expect("Could not find boot hart in FDT")
            .timebase_frequency() as u64;
        hart.timebase_freq.set(timebase_freq);
    });

    info!("initialized paging");

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
    // SAFETY: We guarantee that this virtual address is well-aligned and unaliased.
    unsafe { HEAP_ALLOCATOR.init(Virtual::from_usize(KHEAP_VMEM_OFFSET)) };

    let plic = fdt
        .find_compatible(&["riscv,plic0"])
        .expect("Could not find a compatible PLIC");
    let plic_base_phys = PhysicalMut::<u8, DirectMapped>::from_ptr(
        plic.reg()
            .unwrap()
            .next()
            .unwrap()
            .starting_address
            .cast_mut(),
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

    // let mut cursor = Cursor::new(BASIC_ELF);
    // let mut elf = Elf::parse_header(&mut cursor).unwrap();
    // elf.parse_sections(&mut cursor).unwrap();
    // elf.parse_segments(&mut cursor).unwrap();

    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

    let captbl_mem = {
        let mut pma = PMAlloc::get();
        let page = pma
            .allocate(kalloc::phys::what_order(1 << (16 + 6)))
            .unwrap();
        let page_virt = page.into_virt();
        {
            // SAFETY: PMAlloc guarantees this is safe
            let slice =
                unsafe { slice::from_raw_parts_mut(page_virt.into_ptr_mut(), 1 << (16 + 6)) };
            slice.fill(0);
        }
        page_virt
    };
    //println!("{:#p}", captbl_mem);
    assert_eq!(captbl_mem.into_usize() & ((1 << 9) - 1), 0, "oops!");

    // let (ut_sz_log2, ut_mem) = {
    //     let mut pma = PMAlloc::get();
    //     let order = pma.num_free_pages().ilog2();
    //     (order + 4096u64.ilog2(), pma.allocate(order).unwrap())
    // };
    // SAFETY: This memory is valid.
    let captbl = unsafe { Captbl::new(captbl_mem, 16) };

    {
        let slot = captbl.get_mut::<EmptySlot>(1).unwrap();
        slot.replace(Captbl::clone(&captbl));
        let slot = captbl.get_mut::<EmptySlot>(4).unwrap();
        slot.replace(AllocatorSlot);
        let page = {
            let mut pma = PMAlloc::get();
            pma.allocate(kalloc::phys::what_order(4.kib())).unwrap()
        };
        let slot = captbl.get_mut::<EmptySlot>(2).unwrap();
        // SAFETY: This page is valid.
        slot.replace(unsafe { Page::new(page, 12) });
        //let slot = captbl.get_mut::<EmptySlot>(4).unwrap();
        // SAFETY: This memory is valid.
        //slot.replace(unsafe { Untyped::new(ut_mem, ut_sz_log2 as u8) });
    }

    //println!("captbl: {:#x?}", captbl);

    // let mut proc = Proc::new("basic_elf", captbl);
    // {
    //     let private = proc.private_mut();
    //     // TODO: what does xv6 even use this for
    //     private.mem_size = usize::MAX;

    //     // SAFETY: We have exclusive access to this process
    //     let trapframe = unsafe { &mut *private.trapframe };
    //     let trapframe = trapframe.write(Trapframe::default());

    //     trapframe.user_epc = elf.entry_addr as u64;

    //     trapframe.sp = private
    //         .mman
    //         .alloc(
    //             &RegionRequest {
    //                 n_pages: 4.mib() / 4.kib(),
    //                 contiguous: true,
    //                 flags: PageTableFlags::VAD | PageTableFlags::RW | PageTableFlags::USER,
    //                 purpose: RegionPurpose::Stack,
    //             },
    //             Some(VirtualConst::from_usize(paging::LOWER_HALF_TOP)),
    //         )
    //         .unwrap()
    //         .end
    //         .into_usize() as u64;

    //     let mut cursor2 = Cursor::new(BASIC_ELF);
    //     cursor2
    //         .seek(SeekFrom::Start(cursor.seek(SeekFrom::Current(0)).unwrap()))
    //         .unwrap();
    //     for seg in elf.parse_segments(&mut cursor2).unwrap() {
    //         let seg = seg.unwrap();
    //         if seg.seg_type == SegmentType::Load {
    //             let start_addr = VirtualConst::<u8, Identity>::from_usize(seg.virt_addr);
    //             let start_addr_pgalign = start_addr.page_align();
    //             let offset_from_pg = start_addr.into_usize() - start_addr_pgalign.into_usize();
    //             let end_addr =
    //                 VirtualConst::<u8, Identity>::from_usize(seg.virt_addr + seg.mem_size)
    //                     .add(4.kib())
    //                     .page_align();
    //             let n_pages = (end_addr.into_usize() - start_addr_pgalign.into_usize()) / 4.kib();
    //             // println!("seg.virt_addr = {:#x}, seg.virt_addr + seg.mem_size = {:#x}, start_addr={:#x}, end_addr={:#x}",
    //             //     seg.virt_addr,
    //             //     seg.virt_addr + seg.mem_size,
    //             //     start_addr.into_usize(),
    //             //     end_addr.into_usize()
    //             // );

    //             let mut flags = PageTableFlags::VAD | PageTableFlags::USER;
    //             if seg.flags.contains(SegmentFlags::READ) {
    //                 flags |= PageTableFlags::READ;
    //             }
    //             if seg.flags.contains(SegmentFlags::WRITE) {
    //                 flags |= PageTableFlags::WRITE;
    //             }
    //             if seg.flags.contains(SegmentFlags::EXEC) {
    //                 flags |= PageTableFlags::EXECUTE;
    //             }

    //             // TODO: find purpose
    //             private
    //                 .mman
    //                 .alloc(
    //                     &RegionRequest {
    //                         n_pages,
    //                         contiguous: true,
    //                         flags,
    //                         purpose: RegionPurpose::Unknown,
    //                     },
    //                     Some(start_addr_pgalign),
    //                 )
    //                 .unwrap();

    //             cursor.seek(SeekFrom::Start(seg.offset as u64)).unwrap();
    //             let mut len = seg.file_size;
    //             let mut addr = start_addr;
    //             // TODO: WHAT THE FUCK IS THIS????????
    //             // TODO: WHAT WHAT WHAT WHAT WHAT
    //             // TODO: I HAVE NO IDEA WHY THIS WORKS
    //             // TODO: THIS IS ALMOST CERTAINLY BUGRIDDEN
    //             loop {
    //                 if len == 0 {
    //                     break;
    //                 }
    //                 let n = cmp::min(4.kib() - offset_from_pg, len);

    //                 let table = private.mman.get_table();

    //                 let phys_addr = table.walk(addr).unwrap().0;
    //                 let virt_addr = phys_addr.into_virt();

    //                 // SAFETY: This page was allocated by the
    //                 // UserMemoryManager and is 4 KiB long, and len <=
    //                 // n <= min(4KiB, len_remaining).
    //                 let buf = unsafe { slice::from_raw_parts_mut(virt_addr.into_ptr_mut(), n) };
    //                 cursor.read(buf).unwrap();
    //                 addr = addr.add(n);

    //                 len -= n;
    //             }
    //             // for page in 0..n_pages {
    //             //     if len == 0 {
    //             //         break;
    //             //     }
    //             //     let n = cmp::min(4.kib(), len);

    //             //     let phys_addr = private
    //             //         .mman
    //             //         .get_table()
    //             //         .walk(start_addr_pgalign.add(page * 4.kib()))
    //             //         .unwrap()
    //             //         .0;
    //             //     let virt_addr = phys_addr.into_virt();

    //             //     // SAFETY: This page was allocated by the
    //             //     // UserMemoryManager and is 4 KiB long, and len <=
    //             //     // n <= 4 KiB.
    //             //     let buf = unsafe { slice::from_raw_parts_mut(virt_addr.into_ptr_mut(), n) };
    //             //     cursor.read(buf).unwrap();
    //             //     crate::println!("n={n:x?}, len={len:x?}, phys_addr={phys_addr:x?}, virt_addr={virt_addr:x?}");
    //             //     crate::println!("buf={buf:x?}");

    //             //     len -= n;
    //             // }
    //         }
    //     }

    //     let slot = private.captbl.get_mut::<EmptySlot>(3).unwrap();
    //     slot.replace(PgTbl::new(private.mman.get_table().clone()));

    //     proc.set_state(ProcState::Runnable);
    // }

    let init_padded = INIT.len() + (INIT.len() % 4.kib());
    let page = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(init_padded + 4.mib()))
            .expect("allocate init process")
    };
    {
        let data = page.add(4.mib());
        // SAFETY: We have just allocated this ptr, and it is valid for at least INIT.len.
        let s = unsafe { slice::from_raw_parts_mut(data.into_virt().into_ptr_mut(), INIT.len()) };
        s.copy_from_slice(INIT);
    }

    // SAFETY: The page is zeroed and thus is well-defined and valid.
    let pgtbl =
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) };
    let pgtbl = Arc::new(SpinRwLock::new(pgtbl));
    let pgtbl = SharedPageTable::from_inner(pgtbl);
    let proc = Thread::new(String::from("user_mode_woo"), Some(captbl), pgtbl);
    {
        let mut private = proc.private.write();

        {
            let slot = private
                .captbl
                .as_ref()
                .unwrap()
                .get_mut::<EmptySlot>(3)
                .unwrap();
            slot.replace(PgTbl::new(private.root_pgtbl.clone()));
        }
        let mut trapframe = proc.trapframe.lock();
        trapframe.user_epc = 0x1040_0000;
        trapframe.sp = 0x1040_0000;
        for i in 0..(init_padded / 4.kib() + (4.mib() / 4.kib())) {
            private.root_pgtbl.map(
                page.add(i * 4.kib()).into_identity().into_const(),
                VirtualConst::from_usize(0x1000_0000).add(i * 4.kib()),
                PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RWX,
                PageSize::Base,
            );
        }

        proc.state.store(ThreadState::Runnable, Ordering::Relaxed);
    }
    // let mut proc = Proc::new(String::from("user_mode_woo"));
    // #[allow(clippy::undocumented_unsafe_blocks)]
    // {
    //     let private = proc.private_mut();
    //     private.mem_size = 4.kib();
    //     let trapframe = unsafe { &mut *private.trapframe };
    //     let mut trapframe = trapframe.write(Trapframe::default());
    //     trapframe.user_epc = 0;
    //     trapframe.sp = 4.kib();
    //     private.pgtbl.map(
    //         VirtualConst::<u8, Kernel>::from_usize(fn_user_code_woo().into_usize())
    //             .into_phys()
    //             .into_identity(),
    //         VirtualConst::from_usize(0),
    //         PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RX,
    //     );
    //     proc.set_state(ProcState::Runnable);
    // };
    // let mut proc2 = Proc::new(String::from("user_mode_woo2"));
    // #[allow(clippy::undocumented_unsafe_blocks)]
    // {
    //     let private = proc2.private_mut();
    //     private.mem_size = 4.kib();
    //     let trapframe = unsafe { &mut *private.trapframe };
    //     let trapframe = trapframe.write(Trapframe::default());
    //     trapframe.user_epc = 0x00;
    //     trapframe.sp = 4.kib();
    //     private.mman.map_direct(
    //         VirtualConst::<u8, Kernel>::from_usize(fn_user_code_woo().into_usize())
    //             .into_phys()
    //             .into_identity(),
    //         VirtualConst::from_usize(0),
    //         PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RX,
    //     );
    //     proc2.set_state(ProcState::Runnable);
    // }

    LOCAL_HART.with(|hart| {
        //hart.push_off();

        info!("{}", r"  ___   .  _  _ __   ___".bright_purple());
        info!("{}", r" /  /  /  /  / /__| /  /".bright_purple());
        info!("{}", r"/   \_/\__\_/_/\___/   \".bright_purple());
        info!(
            "{} {} {}",
            "=====".bright_white().bold(),
            "river v0.0.-Inf".bright_magenta().bold(),
            "====".bright_white().bold(),
        );
        info!("boot hart id: {}", hart.hartid.get());

        let (free, total) = {
            let pma = PMAlloc::get();
            (pma.num_free_pages(), pma.num_pages())
        };
        let used = total - free;
        let free = (free * 4.kib()) / 1.mib();
        let used = (used * 4.kib()) / 1.mib();
        let total = (total * 4.kib()) / 1.mib();
        info!(
            "current memory usage: {}MiB / {}MiB ({}MiB free)",
            used, total, free
        );
        // panic!("{:#?}", asm::get_pagetable());

        let mut scheduler_map = BTreeMap::new();
        scheduler_map.insert(hart.hartid.get(), SpinMutex::new(SchedulerInner::default()));
        for hart in fdt
            .cpus()
            .filter(|cpu| cpu.ids().first() != hart.hartid.get() as usize)
        {
            let id = hart.ids().first();
            info!("found FDT node for hart {id}");
            // Allocate a 4 MiB stack
            let stack_base_phys = {
                let mut pma = PMAlloc::get();
                pma.allocate(kalloc::phys::what_order(4.mib()))
                    .expect("Could not allocate stack for hart")
            };
            let sp_phys = stack_base_phys.add(4.mib());

            let boot_data = Box::into_raw(Box::new(HartBootData {
                // We're both in the kernel--this hart will use the same SATP as us.
                raw_satp: asm::get_satp(),
                sp_phys,
                fdt_ptr_virt: VirtualConst::<_, DirectMapped>::from_ptr(fdt_ptr),
            }));
            let boot_data_virt = VirtualConst::<_, Identity>::from_ptr(boot_data);
            let boot_data_phys = root_page_table()
                .lock()
                .walk(boot_data_virt.cast::<u8>())
                .unwrap()
                .0
                .into_usize();

            let start_hart = boot::start_hart as *const u8;
            let start_hart_addr = VirtualConst::<_, Kernel>::from_ptr(start_hart)
                .into_phys()
                .into_usize();

            scheduler_map.insert(id as u64, SpinMutex::new(SchedulerInner::default()));

            sbi::hsm::hart_start(id, start_hart_addr, boot_data_phys)
                .expect("Could not start hart");
        }

        Scheduler::init(scheduler_map);
        Scheduler::enqueue(proc);

        // Scheduler::enqueue(proc2);
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
        //sbi::timer::set_timer(5 * timebase_freq).unwrap();
    });
    N_STARTED.fetch_add(1, Ordering::Relaxed);

    while N_STARTED.load(Ordering::Relaxed) != fdt.cpus().count() {
        core::hint::spin_loop();
    }
    // SAFETY: The scheduler is only started once on the main hart.
    unsafe { Scheduler::start() }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain_hart"]
extern "C" fn kmain_hart(fdt_ptr: *const u8) -> ! {
    // SAFETY: The pointer given to us is guaranteed to be valid by our caller
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(e) => panic!("error loading fdt: {e}"),
    };

    LOCAL_HART.with(|hart| {
        let timebase_freq = fdt
            .cpus()
            .find(|cpu| cpu.ids().first() == hart.hartid.get() as usize)
            .expect("Could not find boot hart in FDT")
            .timebase_frequency() as u64;
        hart.timebase_freq.set(timebase_freq);
    });

    info!("hart {} starting", hartid());

    let uart = fdt.chosen().stdout().unwrap();
    let uart_irq = uart.interrupts().unwrap().next().unwrap() as u32;
    //PLIC.set_priority(uart_irq, 1);
    PLIC.hart_senable(uart_irq);
    PLIC.hart_set_spriority(0);
    // SAFETY: The address provided is valid.
    unsafe { asm::write_stvec(trap::kernel_trapvec as *const u8) };

    asm::software_intr_on();
    asm::timer_intr_on();
    asm::external_intr_on();

    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

    LOCAL_HART.with(|hart| {
        let _timebase_freq = fdt
            .cpus()
            .find(|cpu| cpu.ids().first() == hart.hartid.get() as usize)
            .expect("Could not find hart in FDT")
            .timebase_frequency() as u64;

        // // About 1/10 sec.
        // hart.timer_interval.set(timebase_freq / 10);
        // let interval = hart.timer_interval.get();
        // sbi::timer::set_timer(asm::read_time() + interval).unwrap();
    });

    N_STARTED.fetch_add(1, Ordering::Relaxed);

    loop {
        asm::nop();
    }
    // SAFETY: The scheduler is only started once on the main hart.
    // unsafe { Scheduler::start() }
}

#[panic_handler]
pub fn panic_handler(panic_info: &PanicInfo) -> ! {
    // Forcibly unlock the UART device, so we don't accidentally
    // deadlock here. SAFETY: This panic handler will never return to
    // the code that locked the UART device.
    //
    // FIXME: Deadlock in the panic handler somehow causes traps. Not
    // sure why. MAYBEDONTFIXME: I know why. It's cause the deadlock
    // detector panics if it fails. So if we panic here we trap, for
    // some reason. That needs fixing, potentially. Or maybe not.
    unsafe { UART.force_unlock() };

    if paging::enabled() {
        // Paging is set up, we can use dynamic dispatch.
        if let Some(msg) = panic_info.message() {
            critical!("panic occurred: {}", msg);
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
            critical!("panic occurred (reason unknown).\n");
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
