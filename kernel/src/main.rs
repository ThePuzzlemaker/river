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
    get_many_mut,
    thread_local,
    stmt_expr_attributes
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
pub mod hart_local;
pub mod kalloc;
pub mod paging;
pub mod plic;
pub mod proc;
pub mod sched;
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
    arch::asm,
    mem::MaybeUninit,
    panic::PanicInfo,
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};

use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc};
use asm::hartid;
use fdt::Fdt;
use log::{error, info, trace};
use owo_colors::OwoColorize;
use paging::{root_page_table, PageTableFlags};
use rille::{
    addr::{DirectMapped, Identity, Kernel, Physical, PhysicalMut, Virtual, VirtualConst},
    capability::{paging::PageSize, Captr, CaptrRange, Empty},
    init::BootInfo,
    symbol,
    units::StorageUnits,
};
use uart::UART;

use crate::{
    asm::InterruptDisabler,
    boot::HartBootData,
    capability::{captbl::Captbl, global_interrupt_pool, Page, Thread, ThreadState},
    hart_local::LOCAL_HART,
    kalloc::{linked_list::LinkedListAlloc, phys::PMAlloc},
    paging::{PagingAllocator, SharedPageTable},
    plic::PLIC,
    sched::{Scheduler, SchedulerInner},
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

pub static INIT: &[u8] = include_bytes!(env!("CARGO_BUILD_INIT_PATH"));

static N_STARTED: AtomicUsize = AtomicUsize::new(0);
pub static N_HARTS: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[link_section = ".init.kmain"]
extern "C" fn kmain(fdt_ptr: *const u8) -> ! {
    // SAFETY: Hart-local data has not been initialized at all as of
    // yet.
    unsafe { hart_local::init() }
    let intr = InterruptDisabler::new();

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

    syslog::init_logging();
    //    syslog::parse_log_filter(Some("river::trap=trace"));
    trace!("syslog initialized!");

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
    PLIC.hart_set_spriority(0);
    // SAFETY: The trap vector address is valid.
    unsafe { asm::write_stvec(trap::kernel_trapvec as *const u8) }

    IRQS.get_or_init(|| Irqs { uart: uart_irq });

    asm::software_intr_on();
    asm::timer_intr_on();
    asm::external_intr_on();

    drop(intr);
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

    // SAFETY: We have just initialized this memory and it is valid for this amount of slots.
    let captbl = unsafe { Captbl::new(captbl_mem, 16) };

    {
        let slot = captbl.get_mut::<Empty>(1).unwrap();
        slot.replace(Captbl::clone(&captbl).downgrade());
    }

    let init_padded = INIT.len().next_multiple_of(4.kib());
    let init = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(init_padded + 4.mib()))
            .expect("allocate init process")
    };
    {
        let data = init.add(4.mib());
        // SAFETY: We have just allocated this ptr, and it is valid for at least INIT.len.
        let s = unsafe { slice::from_raw_parts_mut(data.into_virt().into_ptr_mut(), INIT.len()) };
        s.copy_from_slice(INIT);
    }

    // SAFETY: The page is zeroed and thus is well-defined and valid.
    let pgtbl =
        unsafe { Box::<MaybeUninit<_>, _>::assume_init(Box::new_uninit_in(PagingAllocator)) };
    let pgtbl = Arc::new(SpinRwLock::new(pgtbl));
    let pgtbl = SharedPageTable::from_inner(pgtbl);
    let proc = Thread::new(String::from("user_mode_woo"), Some(captbl), Some(pgtbl));
    let mut free_slot_ctr = 7;
    let mut init_pages_range = 0..0;
    proc.setup_page_table();
    {
        let mut private = proc.private.lock();

        let trampoline_virt =
            VirtualConst::<u8, Kernel>::from_usize(symbol::trampoline_start().into_usize());

        private.root_pgtbl.as_mut().unwrap().map(
            trampoline_virt.into_phys().into_identity(),
            VirtualConst::from_usize(usize::MAX - 4.kib() + 1),
            PageTableFlags::VAD | PageTableFlags::RX,
            PageSize::Base,
        );

        {
            let slot = private
                .captbl
                .as_ref()
                .unwrap()
                .get_mut::<Empty>(2)
                .unwrap();
            slot.replace(private.root_pgtbl.clone().unwrap());
        }
        let mut trapframe = proc.trapframe.lock();
        trapframe.user_epc = 0x1040_0000;
        trapframe.sp = 0x1040_0000;
        trapframe.a0 = 0x2000_0000;
        init_pages_range.start = free_slot_ctr;
        for i in 0..(init_padded / 4.kib() + (4.mib() / 4.kib())) {
            let phys = init.add(i * 4.kib());
            private.root_pgtbl.as_mut().unwrap().map(
                phys.into_identity().into_const(),
                VirtualConst::from_usize(0x1000_0000).add(i * 4.kib()),
                PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::RWX,
                PageSize::Base,
            );

            {
                let slot = private
                    .captbl
                    .as_ref()
                    .unwrap()
                    .get_mut::<Empty>(free_slot_ctr)
                    .unwrap();
                // SAFETY: This page is valid for 1 page size (12 address bits)
                // TODO - this isn't entirely valid -- PMA dealloc
                slot.replace(unsafe { Page::new(phys, 12) });
            }
            free_slot_ctr += 1;
        }
        init_pages_range.end = free_slot_ctr;

        private.state = ThreadState::Runnable;
    }

    {
        let private = proc.private.lock();
        let slot = private
            .captbl
            .as_ref()
            .unwrap()
            .get_mut::<Empty>(3)
            .unwrap();
        slot.replace(proc.clone());
        let slot = private
            .captbl
            .as_ref()
            .unwrap()
            .get_mut::<Empty>(6)
            .unwrap();
        // TODO: make sure interrupt handlers/pools can't be copied in slots, only moved.
        slot.replace(global_interrupt_pool().clone());
    }

    let fdt_size = fdt.total_size().next_multiple_of(4.kib());

    let fdt_init = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(fdt_size)).unwrap()
    };

    {
        // SAFETY: We have just allocated this memory, and it is valid to be zeroed.
        let s = unsafe {
            slice::from_raw_parts_mut(fdt_init.into_virt().into_ptr_mut(), fdt.total_size())
        };
        // SAFETY: We know the FDT is valid by our invariants.
        s.copy_from_slice(unsafe { slice::from_raw_parts(fdt_ptr, fdt.total_size()) });
    }

    let mut fdt_pages_range = 0..0;
    {
        let mut private = proc.private.lock();

        fdt_pages_range.start = free_slot_ctr;
        for i in 0..(fdt_size / 4.kib()) {
            let phys = fdt_init.add(i * 4.kib());
            private.root_pgtbl.as_mut().unwrap().map(
                phys.into_identity().into_const(),
                VirtualConst::from_usize(0x2000_1000).add(i * 4.kib()),
                PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::READ,
                PageSize::Base,
            );

            let slot = private
                .captbl
                .as_ref()
                .unwrap()
                .get_mut::<Empty>(free_slot_ctr)
                .unwrap();
            // SAFETY: This page is valid for 1 page size (12 address bits)
            // TODO: this isn't entirely valid--PMA dealloc
            slot.replace(unsafe { Page::new(phys, 12) });
            free_slot_ctr += 1;
        }
        fdt_pages_range.end = free_slot_ctr;
    }

    let bootinfo_page = {
        let mut pma = PMAlloc::get();
        pma.allocate(kalloc::phys::what_order(4.kib())).unwrap()
    };
    {
        let mut private = proc.private.lock();
        private.root_pgtbl.as_mut().unwrap().map(
            bootinfo_page.into_const().into_identity(),
            VirtualConst::from_usize(0x2000_0000),
            PageTableFlags::VAD | PageTableFlags::USER | PageTableFlags::READ,
            PageSize::Base,
        );
        let slot = private
            .captbl
            .as_ref()
            .unwrap()
            .get_mut::<Empty>(5)
            .unwrap();
        // SAFETY: This page is valid for 1 page size (12 address bits)
        // TODO: this isn't entirely valid -- PMA dealloc
        slot.replace(unsafe { Page::new(bootinfo_page, 12) });
    }

    let mut dev_pages_range = 0..0;
    {
        dev_pages_range.start = free_slot_ctr;
        let private = proc.private.lock();
        let slot = private
            .captbl
            .as_ref()
            .unwrap()
            .get_mut::<Empty>(free_slot_ctr)
            .unwrap();
        // SAFETY: This memory is device memory and is valid for a page.
        slot.replace(unsafe {
            Page::new_device(
                PhysicalMut::from_ptr(
                    uart.reg()
                        .unwrap()
                        .next()
                        .unwrap()
                        .starting_address
                        .cast_mut(),
                ),
                12,
            )
        });
        free_slot_ctr += 1;
        dev_pages_range.end = free_slot_ctr;
    }

    let boot_info = BootInfo {
        captbl_size_log2: 16,
        init_pages: CaptrRange {
            // SAFETY: We know these slots contain pages.
            lo: unsafe { Captr::from_raw_unchecked(init_pages_range.start) },
            // SAFETY: See above.
            hi: unsafe { Captr::from_raw_unchecked(init_pages_range.end) },
        },
        fdt_pages: CaptrRange {
            // SAFETY: We know these slots contain pages.
            lo: unsafe { Captr::from_raw_unchecked(fdt_pages_range.start) },
            // SAFETY: See above.
            hi: unsafe { Captr::from_raw_unchecked(fdt_pages_range.end) },
        },
        dev_pages: CaptrRange {
            // SAFETY: We know these slots contain pages.
            lo: unsafe { Captr::from_raw_unchecked(dev_pages_range.start) },
            // SAFETY: See above.
            hi: unsafe { Captr::from_raw_unchecked(dev_pages_range.end) },
        },
        free_slots: CaptrRange {
            // SAFETY: We know this slot is free.
            lo: unsafe { Captr::from_raw_unchecked(free_slot_ctr) },
            // SAFETY: See above.
            hi: unsafe { Captr::from_raw_unchecked(1 << 16) },
        },
        fdt_ptr: 0x2000_1000 as *const u8,
    };

    // SAFETY: We know this page is valid
    unsafe {
        bootinfo_page
            .into_virt()
            .into_ptr_mut()
            .cast::<BootInfo>()
            .write(boot_info);
    }

    N_HARTS.store(fdt.cpus().count(), Ordering::Relaxed);

    LOCAL_HART.with(move |hart| {
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
        info!("timebase freq: {}", hart.timebase_freq.get());

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
        Scheduler::enqueue_dl(&proc, None, &mut proc.private.lock());
        drop(proc);

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
    // SAFETY: Hart-local data has not been initialized at all as of
    // yet.
    unsafe { hart_local::init() }
    let intr = InterruptDisabler::new();

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

    PLIC.hart_set_spriority(0);
    // SAFETY: The address provided is valid.
    unsafe { asm::write_stvec(trap::kernel_trapvec as *const u8) };

    asm::software_intr_on();
    asm::timer_intr_on();
    asm::external_intr_on();

    drop(intr);
    // Now that the PLIC is set up, it's fine to interrupt.
    asm::intr_on();

    LOCAL_HART.with(|hart| {
        let timebase_freq = fdt
            .cpus()
            .find(|cpu| cpu.ids().first() == hart.hartid.get() as usize)
            .expect("Could not find hart in FDT")
            .timebase_frequency() as u64;

        // About 1/10 sec.
        hart.timer_interval.set(timebase_freq / 10);
        let interval = hart.timer_interval.get();
        sbi::timer::set_timer(asm::read_time() + interval).unwrap();
    });

    N_STARTED.fetch_add(1, Ordering::Relaxed);

    while N_STARTED.load(Ordering::Relaxed) != fdt.cpus().count() {
        core::hint::spin_loop();
    }

    // loop {
    //     asm::nop();
    // }
    // SAFETY: The scheduler is only started once on the main hart.
    unsafe { Scheduler::start() }
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
            error!("panic occurred: {}", msg);
            if let Some(location) = panic_info.location() {
                println!(
                    "  at {}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                );
            }
        } else {
            error!("panic occurred (reason unknown).\n");
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
