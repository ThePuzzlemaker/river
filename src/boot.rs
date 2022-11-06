// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2021 The vanadinite developers, 2022 ThePuzzlemaker
//
// This Source Code Form is subject to the terms of the Mozilla Public License,
// v. 2.0. If a copy of the MPL was not distributed with this file, You can
// obtain one at https://mozilla.org/MPL/2.0/.
//
// This file contains a large portion of code taken from vanadinite:
// https://github.com/repnop/vanadinite/blob/274659cc79955733147c5d391d0ee01993a2b945/src/kernel/vanadinite/src/mem/phys/mod.rs
// and is thus licensed individually to the rest of the project as MPL-2.0.

use core::{
    arch::{asm, global_asm},
    sync::atomic::Ordering,
};

use fdt::Fdt;

use crate::{
    addr::{
        DirectMapped, Kernel, PhysicalConst, PhysicalMut, VirtualConst, ACTUAL_PHYSICAL_OFFSET,
        PHYSICAL_OFFSET,
    },
    asm::{hartid, tp},
    hart_local,
    kalloc::phys::PMAlloc,
    paging::{PageTable, PageTableFlags, Satp},
    symbol,
    uart::UART,
    units::StorageUnits,
    util::round_up_pow2,
};

global_asm!("
/*
    SPDX-License-Identifier: MPL-2.0
    SPDX-FileCopyrightText: 2021 The vanadinite developers, 2022 ThePuzzlemaker
    
    This Source Code Form is subject to the terms of the Mozilla Public License,
    v. 2.0. If a copy of the MPL was not distributed with this file, You can
    obtain one at https://mozilla.org/MPL/2.0/.

    This file contains a large portion of code taken from vanadinite:
    https://github.com/repnop/vanadinite/blob/274659cc79955733147c5d391d0ee01993a2b945/src/kernel/vanadinite/src/boot/entry.rs
    and is thus licensed individually to the rest of the project as MPL-2.0.
*/
.pushsection .init

.option norvc

.type start, @function
.global start
start:
    # Disable S-mode interrupts
    csrw sie, zero
    csrci sstatus, 2

.option push
.option norelax
    lla gp, __global_pointer$
.option pop

    lla t0, __bss_start
    lla t1, __bss_end

    # Clear the .bss section
clear_bss:
    beq t0, t1, done_clear_bss
    sd zero, (t0)
    addi t0, t0, 8
    j clear_bss
    
done_clear_bss:
    lla sp, __tmp_stack_top

    # Put the hart id into the thread pointer.
    # We do not have TLS yet so the thread pointer is unused otherwise.
    # It's saved/restored on context switches to/from usermode too.
    mv tp, a0

    # We don't care about having the hart id in a0 anymore, so let's move the
    # FDT pointer as well and just give the kernel entry point just one
    # argument.
    mv a0, a1

    tail early_boot

.popsection
");

fn early_trap_handler() -> ! {
    loop {
        crate::nop()
    }
}

/// # Safety
///
/// Here be dragons. This is the kernel entry point.
/// Don't call it unless, well, you're the kernel *just* after boot.
#[no_mangle]
pub unsafe extern "C" fn early_boot(fdt_ptr: *const u8) -> ! {
    // Make sure traps don't cause a bunch of exceptions, by just loop { nop }-ing them.
    unsafe { asm!("csrw stvec, {}", in(reg) early_trap_handler as usize) };
    let fdt: Fdt<'static> = match unsafe { Fdt::from_ptr(fdt_ptr) } {
        Ok(fdt) => fdt,
        Err(_e) => {
            sbi::hart_state_management::hart_stop().unwrap();
            unreachable!()
        }
    };

    let stdout = fdt.chosen().stdout().unwrap();
    let uart_reg = stdout.reg().unwrap().next().unwrap();
    assert!(stdout.compatible().unwrap().all().any(|x| x == "ns16550a"));

    {
        let mut uart = UART.lock();
        unsafe { uart.init(uart_reg.starting_address as *mut u8) };
        uart.print_str_sync("[info] initialized serial device at 0x");
        uart.early_print_u64_hex(uart_reg.starting_address as u64);
        uart.print_str_sync("\n");
    }

    let kernel_start = symbol::kernel_start().into_usize();
    let kernel_end = symbol::kernel_end().into_usize();

    // Find the memory region that contains the kernel
    let memory_region = fdt
        .memory()
        .regions()
        .find(|region| {
            let start = region.starting_address as usize;
            let end = start + region.size.unwrap();

            start <= kernel_start && kernel_end <= end
        })
        .unwrap();

    let start = memory_region.starting_address as usize;
    let size = memory_region.size.unwrap();

    let kernel_end_ptr = kernel_end as *mut u8;

    let pma_start = if fdt_ptr >= kernel_end_ptr {
        // round up to nearest multiple of 4096
        let ptr = fdt_ptr as usize + fdt.total_size();
        round_up_pow2(ptr, 4096) as *mut u8
    } else {
        round_up_pow2(kernel_end_ptr as usize, 4096) as *mut u8
    };

    // fixup size
    let size = size - (pma_start as usize - start);
    let size = size - (size % 4096);
    let size = size.next_power_of_two() >> 1;

    // Initialize the allocator starting at the end of the kernel,
    // and ending at the end of physical memory.
    // SAFETY: pma_start is known to be non-zero and aligned
    // (our kernel cannot be loaded at 0x0).
    // N.B.: This is a bit wasteful. But PMAlloc makes some assumptions about
    // how the memory is laid out and assumes it can clobber a bunch of pages
    // at the start for its bitree.
    // TODO: make this better in the future?
    unsafe { PMAlloc::init(PhysicalMut::from_ptr(pma_start), size) }
    unsafe { hart_local::init() }
    {
        let mut uart = UART.lock();
        uart.print_str_sync("[info] initialized hart-local storage for hart ");
        uart.early_print_u64(hartid());
        uart.print_str_sync("\n");
    }

    let mut root_pgtbl = PageTable::new();

    let bss_start = symbol::bss_start().into_usize();
    let bss_end = symbol::bss_end().into_usize();

    for addr in (bss_start..bss_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::RW | PageTableFlags::VAD,
        )
    }

    let data_start = symbol::data_start().into_usize();
    let data_end = symbol::data_end().into_usize();

    for addr in (data_start..data_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::RW | PageTableFlags::VAD,
        )
    }

    let tmp_stack_start = symbol::tmp_stack_bottom().into_usize();
    let tmp_stack_end = symbol::tmp_stack_top().into_usize();

    // TODO: make this expandable
    for addr in (tmp_stack_start..tmp_stack_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::RW | PageTableFlags::VAD,
        )
    }

    let text_start = symbol::text_start().into_usize();
    let text_end = symbol::text_end().into_usize();

    for addr in (text_start..text_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::READ | PageTableFlags::EXECUTE | PageTableFlags::VAD,
        )
    }

    for addr in 0..64 {
        root_pgtbl.map_gib(
            PhysicalConst::from_usize(addr * 1.gib()),
            VirtualConst::from_usize(ACTUAL_PHYSICAL_OFFSET + addr * 1.gib()),
            PageTableFlags::RW | PageTableFlags::VAD,
        );
    }

    // Leak the root page table so it won't drop.
    let root_pt_phys = root_pgtbl.into_raw().into_phys();

    let satp = Satp {
        asid: 0,
        ppn: root_pt_phys.ppn(),
    };
    let raw_satp = satp.encode();

    // Turn the physical offset mapping to the actual mapping, instead of an identity mapping.
    PHYSICAL_OFFSET.store(ACTUAL_PHYSICAL_OFFSET, Ordering::Relaxed);

    let gp: usize;
    unsafe {
        asm!("lla {}, __global_pointer$", out(reg) gp);
    }

    let tp = tp();

    // Fixup sp, gp, and tp to be in the right address space
    let new_sp = PhysicalMut::<u8, Kernel>::from_usize(tmp_stack_end)
        .into_virt()
        .into_usize();
    let new_gp = PhysicalMut::<u8, Kernel>::from_usize(gp)
        .into_virt()
        .into_usize();
    let new_tp = PhysicalMut::<u8, DirectMapped>::from_usize(tp as usize)
        .into_virt()
        .into_usize();

    let kmain = crate::kmain as *const u8;

    let kmain_virt: VirtualConst<_, Kernel> = PhysicalConst::from_ptr(kmain).into_virt();

    let fdt_ptr = PhysicalConst::<_, DirectMapped>::from_ptr(fdt_ptr)
        .into_virt()
        .into_ptr();

    // Now that we've gotten the panics out of the way, we need to make sure we
    // fix up the serial port address so if anything panics at the start of
    // kmain, it will actually be able to print.
    let serial_paddr: PhysicalMut<u8, DirectMapped> =
        PhysicalConst::from_ptr(uart_reg.starting_address).into_mut();
    let serial_vaddr = serial_paddr.into_virt();

    // Update the UART base w/ its direct-mapped virtual addr
    unsafe { UART.lock().update_serial_base(serial_vaddr.into_ptr_mut()) };

    unsafe {
        asm!(
            "
        csrw stvec, {stvec}

        # set up sp and gp
        mv sp, {new_sp}
        mv gp, {new_gp}
        mv tp, {new_tp}

        csrc sstatus, {mxr}

        # load new `satp`
        csrw satp, {satp}
        sfence.vma
        unimp # trap & go to main
        ",
            mxr = in(reg) 1 << 19,
            satp = in(reg) raw_satp.as_usize(),
            new_sp = in(reg) new_sp,
            new_gp = in(reg) new_gp,
            new_tp = in(reg) new_tp,
            stvec = in(reg) kmain_virt.into_usize(),

            // `kmain` args
            in("a0") fdt_ptr,
            options(noreturn, nostack)
        )
    }
}
