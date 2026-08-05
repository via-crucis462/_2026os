#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <sys/time.h>

#ifndef SYS_statx
#define SYS_statx 291
#endif

#define NS_PER_S 1000000000ULL

static double now_s(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0) {
        if (ts.tv_sec > 2000000000LL || ts.tv_nsec < 0 || ts.tv_nsec >= 1000000000LL) {
            fprintf(stderr, "BAD_CLOCK sec=%lld nsec=%lld\n",
                    (long long)ts.tv_sec, (long long)ts.tv_nsec);
        }
        return (double)ts.tv_sec + (double)ts.tv_nsec / NS_PER_S;
    }
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return (double)tv.tv_sec + (double)tv.tv_usec / 1000000.0;
}

struct statx_buf { char pad[256]; };

static void work_cpu(void) {
    volatile uint64_t x = 0;
    for (long i = 0; i < 200000000L; i++) x += i;
    (void)x;
}

static void work_mixed(void) {
    const char *file = "/work/tgoskits/Cargo.toml";
    char *buf = aligned_alloc(4096, 4096);
    struct statx_buf st;
    for (long i = 0; i < 20000; i++) {
        int fd = open(file, O_RDONLY);
        if (fd < 0) continue;
        syscall(SYS_statx, AT_FDCWD, file, 0, 0x7ff, &st);
        (void)pread(fd, buf, 4096, 0);
        close(fd);
    }
    free(buf);
}

static void work_write(void) {
    char name[64];
    snprintf(name, sizeof(name), "/work/bench-mp-%d.tmp", (int)getpid());
    char *buf = aligned_alloc(4096, 4096);
    memset(buf, 0x33, 4096);
    for (long i = 0; i < 5000; i++) {
        int fd = open(name, O_CREAT | O_RDWR | O_TRUNC, 0644);
        if (fd < 0) continue;
        (void)pwrite(fd, buf, 4096, (i % 1024) * 4096);
        close(fd);
    }
    free(buf);
}

static void work_pf(void) {
    size_t len = 32UL * 1024 * 1024;
    char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p != MAP_FAILED) {
        for (size_t i = 0; i < len; i += 4096) p[i] = 1;
        munmap(p, len);
    }
}

static void work_pid(void) {
    for (long i = 0; i < 1000000; i++) (void)getpid();
}

static void work_clock(void) {
    struct timespec ts;
    for (long i = 0; i < 1000000; i++) (void)clock_gettime(CLOCK_MONOTONIC, &ts);
}

static void work_statx(void) {
    const char *paths[] = {
        "/bin/busybox",
        "/sbin/init",
        "/lib/ld-musl-riscv64.so.1",
        "/usr/bin/bash",
        "/work/tgoskits/Cargo.toml",
        "/bench/sysbench",
        "/bench/sysbench_mp",
        "/bin/sh",
    };
    struct statx_buf st;
    const char *p = paths[(int)(getpid() % 8)];
    for (long i = 0; i < 20000; i++) {
        syscall(SYS_statx, AT_FDCWD, p, 0, 0x7ff, &st);
    }
}

static void work_pread(void) {
    const char *file = "/work/tgoskits/Cargo.toml";
    char *buf = aligned_alloc(4096, 4096);
    int fd = open(file, O_RDONLY);
    for (long i = 0; i < 50000; i++) {
        if (fd >= 0) (void)pread(fd, buf, 4096, 0);
    }
    if (fd >= 0) close(fd);
    free(buf);
}

static void work_fstat(void) {
    const char *file = "/work/tgoskits/Cargo.toml";
    struct stat st;
    int fd = open(file, O_RDONLY);
    for (long i = 0; i < 50000; i++) {
        if (fd >= 0) (void)fstat(fd, &st);
    }
    if (fd >= 0) close(fd);
}

static void work_openclose(void) {
    const char *file = "/work/tgoskits/Cargo.toml";
    for (long i = 0; i < 20000; i++) {
        int fd = open(file, O_RDONLY);
        if (fd >= 0) close(fd);
    }
}

static void work_mprotect(void) {
    size_t len = 1024 * 4096;
    char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return;
    for (long i = 0; i < 20000; i++) {
        size_t off = (i % 256) * 4096;
        mprotect(p + off, 4096,
                 (i & 1) ? PROT_READ | PROT_WRITE : PROT_READ);
    }
    munmap(p, len);
}

static void work_mmap(void) {
    for (long i = 0; i < 20000; i++) {
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) munmap(p, 4096);
    }
}

static void *thread_fn(void *arg) {
    const char *mode = (const char *)arg;
    if (strcmp(mode, "mprotect") == 0) work_mprotect();
    else work_mmap();
    return NULL;
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "mixed";
    int nproc = argc > 2 ? atoi(argv[2]) : 8;
    if (nproc < 1) nproc = 1;
    if (nproc > 64) nproc = 64;

    if (strcmp(mode, "mprotect_t") == 0 || strcmp(mode, "mmap_t") == 0) {
        pthread_t th[64];
        for (int i = 0; i < nproc; i++) {
            pthread_create(&th[i], NULL, thread_fn,
                           (void *)(strcmp(mode, "mprotect_t") == 0
                                        ? "mprotect" : "mmap"));
        }
        double t0 = now_s();
        for (int i = 0; i < nproc; i++) pthread_join(th[i], NULL);
        double dt = now_s() - t0;
        printf("mp_%-6s threads=%d wall=%.3f s  per_slot=%.3f s\n",
               mode, nproc, dt, dt / nproc);
        return 0;
    }

    double t0 = now_s();
    for (int i = 0; i < nproc; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            if (strcmp(mode, "cpu") == 0) work_cpu();
            else if (strcmp(mode, "write") == 0) work_write();
            else if (strcmp(mode, "pf") == 0) work_pf();
            else if (strcmp(mode, "pid") == 0) work_pid();
            else if (strcmp(mode, "clock") == 0) work_clock();
            else if (strcmp(mode, "statx") == 0) work_statx();
            else if (strcmp(mode, "pread") == 0) work_pread();
            else if (strcmp(mode, "fstat") == 0) work_fstat();
            else if (strcmp(mode, "openclose") == 0) work_openclose();
            else if (strcmp(mode, "mprotect") == 0) work_mprotect();
            else if (strcmp(mode, "mmap") == 0) work_mmap();
            else work_mixed();
            _exit(0);
        }
    }
    while (wait(NULL) > 0) {}
    double dt = now_s() - t0;
    printf("mp_%-6s nproc=%d wall=%.3f s  per_slot=%.3f s\n",
           mode, nproc, dt, dt / nproc);
    return 0;
}
