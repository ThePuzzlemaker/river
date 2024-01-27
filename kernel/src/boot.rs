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

use rille::{
    addr::{
        DirectMapped, Kernel, PhysicalConst, PhysicalMut, VirtualConst, ACTUAL_PHYSICAL_OFFSET,
        PHYSICAL_OFFSET,
    },
    units::StorageUnits,
};

use crate::{
    asm::{self, hartid},
    kalloc::phys::PMAlloc,
    paging::{PageTable, PageTableFlags, RawSatp, Satp},
    symbol,
    uart::UART,
    util::round_up_pow2,
};

#[no_mangle]
#[link_section = ".init.early_trapvec"]
extern "C" fn early_trap_handler() -> ! {
    loop {
        asm::nop();
    }
}

#[no_mangle]
#[allow(clippy::too_many_lines)]
unsafe extern "C" fn early_boot(fdt_ptr: *const u8) -> ! {
    // Make sure traps don't cause a bunch of exceptions, by just loop { nop }-ing them.
    // SAFETY: The trap handler address is valid.
    unsafe { asm::write_stvec(early_trap_handler as *const u8) };
    // SAFETY: OpenSBI guarantees the FDT pointer is valid.
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
        // SAFETY: Our caller guarantees that this address is valid.
        unsafe { uart.init(uart_reg.starting_address.cast_mut()) };
        uart.print_str_sync("[         ] [");
        uart.early_print_u64(hartid());
        uart.print_str_sync("] ");
        uart.print_str_sync("[info]: booting hart ");
        uart.early_print_u64(hartid());
        uart.print_str_sync("...\n");
        uart.print_str_sync("[         ] [");
        uart.early_print_u64(hartid());
        uart.print_str_sync("] ");
        uart.print_str_sync("[info]: initialized serial device at 0x");
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

    // Fixup size: make it a pow of 2
    let size = 1 << size.ilog2();

    let kernel_end_ptr = kernel_end as *mut u8;

    let pma_start = if fdt_ptr >= kernel_end_ptr {
        // round up to nearest multiple of 4096
        let ptr = fdt_ptr as usize + fdt.total_size();
        round_up_pow2(ptr, 4096)
    } else {
        round_up_pow2(kernel_end_ptr as usize, 4096)
    };

    // Initialize the allocator starting at the end of the kernel, and
    // ending at the end of physical memory.
    //
    // SAFETY: pma_start is known to be non-zero and aligned (our
    // kernel cannot be loaded at 0x0). We also know the end of phymem
    // can be used to hold the bitree.
    unsafe { PMAlloc::init(PhysicalMut::from_usize(start), size) }

    for addr in (start..pma_start).step_by(4096) {
        let addr: PhysicalMut<_, DirectMapped> = PhysicalMut::from_usize(addr);
        // SAFETY: This memory is actually used.
        unsafe { PMAlloc::get().mark_used(addr, 0) };
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
        );
    }

    let data_start = symbol::data_start().into_usize();
    let data_end = symbol::data_end().into_usize();

    for addr in (data_start..data_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::RW | PageTableFlags::VAD,
        );
    }

    let tmp_stack_start = symbol::tmp_stack_bottom().into_usize();
    let tmp_stack_end = symbol::tmp_stack_top().into_usize();

    for addr in (tmp_stack_start..tmp_stack_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::RW | PageTableFlags::VAD,
        );
    }

    let text_start = symbol::text_start().into_usize();
    let text_end = symbol::text_end().into_usize();

    for addr in (text_start..text_end).step_by(4096) {
        let addr: PhysicalConst<_, Kernel> = PhysicalConst::from_usize(addr);
        root_pgtbl.map(
            addr.into_identity(),
            addr.into_virt().into_identity(),
            PageTableFlags::READ | PageTableFlags::EXECUTE | PageTableFlags::VAD,
        );
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
    // SAFETY: The value being written into GP is valid.
    unsafe {
        asm!("lla {}, __global_pointer$", out(reg) gp);
    }

    // Fixup sp, gp, and tp to be in the right address space
    let new_stack_ptr = PhysicalMut::<u8, Kernel>::from_usize(tmp_stack_end)
        .into_virt()
        .into_usize();
    let new_global_ptr = PhysicalMut::<u8, Kernel>::from_usize(gp)
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
    let serial_phys_addr: PhysicalMut<u8, DirectMapped> =
        PhysicalConst::from_ptr(uart_reg.starting_address).into_mut();
    let serial_virt_addr = serial_phys_addr.into_virt();

    // Update the UART base w/ its direct-mapped virtual addr.
    //
    // SAFETY: This address is valid given that paging is enabled, and the UART
    // code has a countermeasure for any printing before paging is enabled on
    // the current hart.
    unsafe {
        UART.lock()
            .update_serial_base(serial_virt_addr.into_ptr_mut());
    };

    // SAFETY: we're the kernel.
    unsafe {
        asm!(
            "
        csrw stvec, {stvec}

        # set up sp and gp
        mv sp, {new_sp}
        mv gp, {new_gp}

        csrc sstatus, {mxr}

        # load new `satp`
        csrw satp, {satp}
        unimp # trap & go to main
        ",
            mxr = in(reg) 1 << 19,
            satp = in(reg) raw_satp.as_usize(),
            new_sp = in(reg) new_stack_ptr,
            new_gp = in(reg) new_global_ptr,
            stvec = in(reg) kmain_virt.into_usize(),

            // `kmain` args
            in("a0") fdt_ptr,
            options(noreturn, nostack)
        )
    }
}

#[repr(C)]
pub struct HartBootData {
    /* 00 */ pub raw_satp: RawSatp,
    /* 08 */ pub sp_phys: PhysicalMut<u8, DirectMapped>,
    /* 18 */ pub fdt_ptr_virt: VirtualConst<u8, DirectMapped>,
}

global_asm!(
    "
.pushsection .init

.option norvc

.type start, @function
.global start
start:
    // Disable S-mode interrupts
    csrw sie, zero
    csrci sstatus, 2

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
    lla sp, __tmp_stack_top

    // Put the hart id into the thread pointer.
    // We do not have TLS yet so the thread pointer is unused otherwise.
    // It's saved/restored on context switches to/from usermode too.
    mv tp, a0

    // We don't care about having the hart id in a0 anymore, so let's move the
    // FDT pointer as well and just give the kernel entry point just one
    // argument.
    mv a0, a1

    tail early_boot

.popsection
.pushsection .init.hart,\"ax\",@progbits
.option norvc
.type start_hart, @function
.global start_hart
start_hart:
    csrw sie, zero
    csrci sstatus, 2

.option push
.option norelax
    lla gp, __global_pointer$
.option pop
    // Offset of `sp_phys` in `BootHartData` -- make sure we're operating on a
    // stack we can actually use :P
    ld sp, 8(a1)
    mv tp, a0
    mv a0, a1

    tail early_boot_hart

.popsection
    ",
);

extern "C" {
    pub fn start_hart(hart_id: u64, data: u64);
}

#[no_mangle]
unsafe extern "C" fn early_boot_hart(data: PhysicalMut<HartBootData, DirectMapped>) -> ! {
    // Make sure traps don't cause a bunch of exceptions, by just loop { nop }-ing them.
    // SAFETY: The trap handler address is valid.
    unsafe { asm::write_stvec(early_trap_handler as *const u8) };

    {
        let mut uart = UART.lock();
        uart.print_str_sync("[         ] [");
        uart.early_print_u64(hartid());
        uart.print_str_sync("] ");
        uart.print_str_sync("[info]: booting hart ");
        uart.early_print_u64(hartid());
        uart.print_str_sync("...\n");
    }

    let gp: usize;
    // SAFETY: The value being written into GP is valid.
    unsafe {
        asm!("lla {}, __global_pointer$", out(reg) gp);
    }

    // Fixup sp and gp to be in the right address space
    //
    // TODO: stop doing this whole rigmaroll and make this better (add
    // an `into_ptr` to phymem ops)
    //
    // SAFETY: Our caller guarantees this is safe.
    let new_stack_ptr = unsafe { (*data.into_identity().into_virt().into_ptr()).sp_phys }
        .into_virt()
        .into_usize();
    let new_global_ptr = PhysicalMut::<u8, Kernel>::from_usize(gp)
        .into_virt()
        .into_usize();

    let kmain_hart = crate::kmain_hart as *const u8;

    let kmain_hart_virt: VirtualConst<_, Kernel> = PhysicalConst::from_ptr(kmain_hart).into_virt();

    // SAFETY: Our caller guarantees this is safe.
    let fdt_ptr = unsafe { (*data.into_identity().into_virt().into_ptr()).fdt_ptr_virt }.into_ptr();

    // SAFETY: Our caller guarantees this is safe.
    let raw_satp = unsafe { (*data.into_identity().into_virt().into_ptr()).raw_satp };

    // SAFETY: we're the kernel
    unsafe {
        asm!(
            "
        csrw stvec, {stvec}

        # set up sp and gp
        mv sp, {new_sp}
        mv gp, {new_gp}

        csrc sstatus, {mxr}

        # load new `satp`
        csrw satp, {satp}
        unimp # trap & go to main
        ",
            mxr = in(reg) 1 << 19,
            satp = in(reg) raw_satp.as_usize(),
            new_sp = in(reg) new_stack_ptr,
            new_gp = in(reg) new_global_ptr,
            stvec = in(reg) kmain_hart_virt.into_usize(),

            // `kmain_hart` args
            in("a0") fdt_ptr,
            options(noreturn, nostack)
        )
    }
}
