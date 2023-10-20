.section .text
.global _start
.type _start, @function
_start:	
    li s0, 0
.loop:
	
    li a0, 0

    lla a1, hello

    mv a2, x0
    lla t0, hello_end
    lla t1, hello
    sub a2, t0, t1

    ecall

    li a0, 1
    ecall

    j .loop
//    beq s0, x0, .loop
//    li s0, 1
//.loop_spin:
//    j .loop

.section .rodata
hello:
    .byte 0x48,0x65,0x6c,0x6c,0x6f,0x2c,0x20,0x77,0x6f,0x72,0x6c,0x64,0x21,0x0a
hello_end:
