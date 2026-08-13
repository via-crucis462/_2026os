#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

static void *worker(void *arg) {
    (void)arg;
    size_t region = 64UL * 4096;
    char *base = mmap(NULL, region, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return NULL;
    for (long i = 0; i < 30000; i++) {
        size_t off = (size_t)(i % 64) * 4096;
        base[off] = (char)i;
        if ((i & 1) == 0) {
            mprotect(base + off, 4096, PROT_READ);
            mprotect(base + off, 4096, PROT_READ | PROT_WRITE);
        }
        if ((i & 7) == 0) {
            void *q = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (q != MAP_FAILED) munmap(q, 4096);
        }
        if ((i & 31) == 0) {
            char *big = mmap(NULL, 1024UL * 4096, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (big != MAP_FAILED) {
                for (size_t j = 0; j < 1024UL * 4096; j += 4096) big[j] = 1;
                munmap(big, 1024UL * 4096);
            }
        }
    }
    munmap(base, region);
    return NULL;
}

int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 8;
    pthread_t th[64];
    if (n > 64) n = 64;
    for (int i = 0; i < n; i++) pthread_create(&th[i], NULL, worker, NULL);
    for (int i = 0; i < n; i++) pthread_join(th[i], NULL);
    printf("mm_mix threads=%d done\n", n);
    return 0;
}
