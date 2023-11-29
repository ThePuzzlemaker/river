
/*
    SPDX-License-Identifier: MPL-2.0
    SPDX-FileCopyrightText: 2021 The vanadinite developers, 2022 ThePuzzlemaker
    
    This Source Code Form is subject to the terms of the Mozilla Public License,
    v. 2.0. If a copy of the MPL was not distributed with this file, You can
    obtain one at https://mozilla.org/MPL/2.0/.

    This file was originally from vanadinite:
    https://github.com/repnop/vanadinite/blob/671e2690106225f8deae76a63b30cd15053db8b2/src/kernel/vanadinite/lds/virt.lds
    with modifications of my own, and is thus licensed individually to the rest of the project as MPL-2.0.
*/

OUTPUT_ARCH(riscv);
ENTRY(start);

SECTIONS {
    . = 0xFFFFFFD000000000;

    __offset = . - 0x80200000;
    .text : AT(ADDR(.text) - __offset) {
        . = ALIGN(4K);
        PROVIDE(__text_start = .);
        PROVIDE(KERNEL_START = .);
        . = ALIGN(4);
        *(.init);
        . = ALIGN(4);
        *(.init.hart);
        . = ALIGN(4);
        *(.init.kmain);
        . = ALIGN(4);
        *(.init.trapvec);
        . = ALIGN(4);
        *(.init.early_trapvec);
        . = ALIGN(4);
        *(.init.kmain_hart);
        *(.text .text.*);
        . = ALIGN(4K);
	PROVIDE(TRAMPOLINE_START = .);
        *(.init.trampoline);
        . = ALIGN(4K);
        *(.init.user_trap);
        . = ALIGN(4K);
	*(.user_code);
	. = ALIGN(4K);
        PROVIDE(__text_end = .);
    }

    .data : AT(ADDR(.data) - __offset) {
        . = ALIGN(4K);
        PROVIDE(__data_start = .);
        *(.data .data.* .rodata .rodata.*);
	
        . = ALIGN(4K);
    }


    .tdata : AT(ADDR(.tdata) - __offset) {
        . = ALIGN(4K);
	PROVIDE(__tdata_start = .);
	*(.tdata .tdata.*);
	. = ALIGN(4K);
	PROVIDE(__tdata_end = .);
    }

    .tbss : AT(ADDR(.tbss) - __offset) {
        . = ALIGN(4K);
	PROVIDE(__tbss_start = .);
	*(.tbss .tbss.*);
	. = ALIGN(4K);
	PROVIDE(__tbss_end = .);
    }

    . = ALIGN(8);

    .sdata : AT(ADDR(.sdata) - __offset) {
        PROVIDE(__global_pointer$ = .);
        *(.sdata .sdata.*);
	
        PROVIDE(__eh_frame_start = .);
	KEEP(*(.eh_frame .eh_frame.*));
	*(.eh_frame .eh_frame.*);
	PROVIDE(__eh_frame_end = .);
	
        PROVIDE(__eh_frame_hdr_start = .);
	KEEP(*(.eh_framehdr .eh_framehdr.*));
	*(.eh_framehdr .eh_framehdr.*);
	PROVIDE(__eh_frame_hdr_end = .);
        . = ALIGN(4K);
        PROVIDE(__data_end = .);
    }


    /* . = ALIGN(4K); */
    /* .eh_frame_hdr : AT(ADDR(.eh_frame_hdr) - __offset) { */
    /* 	. = ALIGN(4K); */
    /* } */

    /* . = ALIGN(4K); */
    /* .eh_frame : AT(ADDR(.eh_frame) - __offset) { */
    /* 	. = ALIGN(4K); */
    /* } */

    . = ALIGN(4K);
    PROVIDE(__bss_start = .);
    .sbss : AT(ADDR(.sbss) - __offset) {
        *(.sbss .sbss.*);
    }

    .bss : AT(ADDR(.bss) - __offset) {
        *(.bss .bss.*)
    }
    . = ALIGN(4K);
    PROVIDE(__bss_end = .);

    .tmp_stack : AT(ADDR(.tmp_stack) - __offset) {
        . = ALIGN(4K);
        PROVIDE(__tmp_stack_bottom = .);
        . = ALIGN(4K);
        . += 1024 * 1024 * 4;
        . = ALIGN(4K);
        PROVIDE(__tmp_stack_top = .);
    }


    . = ALIGN(4K);
    PROVIDE(KERNEL_END = .);
}
