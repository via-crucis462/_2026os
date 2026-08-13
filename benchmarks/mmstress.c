#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile int g_fail;
static volatile int g_done;
static pthread_mutex_t g_brk_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t g_fixed_lock = PTHREAD_MUTEX_INITIALIZER;

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static void pin_cpu(int cpu) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    sched_setaffinity(0, sizeof(set), &set);
}

/* Phase 1: concurrent mmap(NULL)+munmap (the MAP_STACK/thread stack model) */
static void *work_mmap_anon(void *arg) {
    long n = *(long *)arg;
    for (long i = 0; i < n; i++) {
        size_t len = 4096 * (1 + (i % 16));
        void *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            if (errno == ENOMEM) continue; /* transient under memory pressure */
            g_fail++;
            break;
        }
        /* touch a page so the area gets a real frame */
        ((volatile char *)p)[0] = 1;
        if (munmap(p, len) != 0) {
            g_fail++;
            break;
        }
    }
    return NULL;
}

struct mprotect_arg {
    char *base;
    long n;
    long stride;
};

static void *work_mprotect(void *arg) {
    struct mprotect_arg *a = (struct mprotect_arg *)arg;
    for (long i = 0; i < a->n; i++) {
        size_t off = (size_t)(i % 256) * 4096;
        if (mprotect(a->base + off, 4096,
                     (i & 1) ? (PROT_READ | PROT_WRITE) : PROT_READ) != 0) {
            g_fail++;
            break;
        }
    }
    return NULL;
}

/* Phase 3: concurrent brk/sbrk */
static void *work_brk(void *arg) {
    long n = *(long *)arg;
    for (long i = 0; i < n; i++) {
        /* brk 是进程级资源：真实 malloc 会加锁，这里模拟受保护的用法，
           重点测试内核在串行 brk 下的正确性。 */
        pthread_mutex_lock(&g_brk_lock);
        void *p = sbrk(8192);
        if (p == (void *)-1) {
            pthread_mutex_unlock(&g_brk_lock);
            g_fail++;
            break;
        }
        ((volatile char *)p)[0] = (char)i;
        if (sbrk(-8192) == (void *)-1) {
            pthread_mutex_unlock(&g_brk_lock);
            g_fail++;
            break;
        }
        pthread_mutex_unlock(&g_brk_lock);
    }
    return NULL;
}

/* Phase 4: concurrent page-fault touch on private regions */
static void *work_touch(void *arg) {
    long n = *(long *)arg;
    for (long i = 0; i < n; i++) {
        size_t len = 4 * 1024 * 1024;
        char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            g_fail++;
            break;
        }
        for (size_t j = 0; j < len; j += 4096) {
            p[j] = (char)j;
            if ((j & 0xffff) == 0 && ((volatile char *)p)[j] != (char)j) {
                g_fail++;
            }
        }
        if (munmap(p, len) != 0) {
            g_fail++;
            break;
        }
    }
    return NULL;
}

/* Phase 5: fixed-address collisions */
static void *work_fixed(void *arg) {
    long n = *(long *)arg;
    for (long i = 0; i < n; i++) {
        pthread_mutex_lock(&g_fixed_lock);
        void *p = mmap((void *)0x500000000L, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        if (p == MAP_FAILED) {
            pthread_mutex_unlock(&g_fixed_lock);
            g_fail++;
            break;
        }
        ((volatile char *)p)[0] = 1;
        if (munmap(p, 4096) != 0) {
            pthread_mutex_unlock(&g_fixed_lock);
            g_fail++;
            break;
        }
        pthread_mutex_unlock(&g_fixed_lock);
    }
    return NULL;
}

/* Phase 6: fork COW hammer */
static int fork_cow_round(void) {
    size_t len = 8 * 1024 * 1024;
    char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return 1;
    for (size_t i = 0; i < len; i += 4096) p[i] = (char)i;
    pid_t pid = fork();
    if (pid < 0) return 1;
    if (pid == 0) {
        for (size_t i = 0; i < len; i += 4096) p[i] ^= 0x5a;
        _exit(0);
    }
    int st;
    waitpid(pid, &st, 0);
    for (size_t i = 0; i < len; i += 4096) {
        if (p[i] != (char)i) return 1;
    }
    munmap(p, len);
    return 0;
}

static void run_phase(const char *name, void *(*fn)(void *), long n, int threads) {
    g_fail = 0;
    double t0 = now_s();
    pthread_t th[64];
    if (threads > 64) threads = 64;
    for (int i = 0; i < threads; i++) pthread_create(&th[i], NULL, fn, &n);
    for (int i = 0; i < threads; i++) pthread_join(th[i], NULL);
    double dt = now_s() - t0;
    printf("PHASE %-10s threads=%d wall=%.6f fail=%d\n", name, threads, dt, g_fail);
    fflush(stdout);
}

int main(int argc, char **argv) {
    long n = argc > 1 ? atol(argv[1]) : 10000;
    int threads = argc > 2 ? atoi(argv[2]) : 8;
    printf("mmstress begin n=%ld threads=%d\n", n, threads);
    fflush(stdout);

    run_phase("mmap_anon", work_mmap_anon, n, threads);

    char *shared = mmap(NULL, 1024 * 1024, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) {
        printf("mmstress shared mmap failed errno=%d\n", errno);
        return 1;
    }
    struct mprotect_arg mp;
    mp.base = shared;
    mp.n = n / 4;
    g_fail = 0;
    double t0 = now_s();
    pthread_t th[64];
    for (int i = 0; i < threads; i++) pthread_create(&th[i], NULL, work_mprotect, &mp);
    for (int i = 0; i < threads; i++) pthread_join(th[i], NULL);
    double dt = now_s() - t0;
    printf("PHASE %-10s threads=%d wall=%.6f fail=%d\n", "mprotect", threads, dt, g_fail);
    fflush(stdout);

    run_phase("brk", work_brk, n / 8, threads);
    run_phase("touch", work_touch, n / 4, threads);
    run_phase("fixed", work_fixed, n / 8, threads);

    g_fail = 0;
    t0 = now_s();
    for (long i = 0; i < n / 8; i++) {
        if (fork_cow_round()) g_fail++;
    }
    dt = now_s() - t0;
    printf("PHASE %-10s threads=%d wall=%.6f fail=%d\n", "fork_cow", 1, dt, g_fail);
    fflush(stdout);

    printf("mmstress end\n");
    return g_fail ? 2 : 0;
}
