.section .text
.global _start
.type _start, @function
.option norvc
li a0, 0xFFFF0001
ecall
nop
j -4
