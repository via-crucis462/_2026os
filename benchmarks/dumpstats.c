#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static long raw_syscall(long n, long a0) {
    register long a7 __asm__("a7") = n;
    register long a0r __asm__("a0") = a0;
    __asm__ volatile("ecall" : "+r"(a0r) : "r"(a7) : "memory");
    return a0r;
}

int main(int argc, char **argv) {
    long n = argc > 1 ? atol(argv[1]) : 999;
    long a0 = argc > 2 ? atol(argv[2]) : 0;
    raw_syscall(n, a0);
    return 0;
}
