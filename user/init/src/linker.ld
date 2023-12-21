OUTPUT_ARCH(riscv);
ENTRY(_start);

SECTIONS {
    . = 0x10000000;
    .stack : {
        PROVIDE(__stack_bottom = .);
        . += 1024 * 1024 * 4;
        PROVIDE(__stack_top = .);	 
    }

    . = 0x10400000;
    .text : {
        . = ALIGN(4K);
	*(.init);
	. = ALIGN(8);
	. = ALIGN(4K);
        *(.text .text.*);
	. = ALIGN(4K);
    }
    .data : {
        . = ALIGN(4K);
        *(.data .data.* .rodata .rodata.*);
	. = ALIGN(4K);
    }

    . = ALIGN(8);
    .sdata : {
        PROVIDE(__global_pointer$ = .);
	*(.sdata .sdata.*);
	. = ALIGN(4K);
    	PROVIDE(__bss_start = .);
	*(.sbss .sbss.*);
	*(.bss .bss.*);
	. = ALIGN(4K);
	PROVIDE(__bss_end = .);
    }
    
    /DISCARD/ : { *(.eh_frame_hdr .eh_frame) }
}
	       
