.option norvc
.section .text
.globl _start
.type _start, @function
_start:
    addi sp, sp, -16
    li t0, 0x554e494c20534f41
    sd t0, 0(sp)
    li t0, 0x0a74696e692f2058
    sd t0, 8(sp)
    li a0, 1
    mv a1, sp
    li a2, 16
    li a7, 64
    ecall

    li a0, 0xfee1dead
    li a1, 0x28121969
    li a2, 0x4321fedc
    li a3, 0
    li a7, 142
    ecall

1:
    wfi
    j 1b
.size _start, . - _start
