    .section .text
    .globl _start
    // 直接触发一个异常
_start:
    syscall 0
    rtn