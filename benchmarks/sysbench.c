#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
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

/* riscv64 generic syscall numbers used directly where glibc has no wrapper */
#ifndef SYS_statx
#define SYS_statx 291
#endif
#ifndef SYS_getdents64
#define SYS_getdents64 61
#endif
#ifndef SYS_futex
#define SYS_futex 98
#endif
#ifndef SYS_readlinkat
#define SYS_readlinkat 78
#endif

#define NS_PER_S 1000000000ULL

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / NS_PER_S;
}

static void report(const char *name, long iters, double t0) {
    double dt = now_s() - t0;
    double per = dt * NS_PER_S / (double)iters;
    printf("%-18s %10ld iters %9.3f s  %10.1f ns/iter\n",
           name, iters, dt, per);
}

struct statx_buf {
    uint32_t stx_mask;
    uint32_t stx_blksize;
    uint64_t stx_attributes;
    uint32_t stx_nlink;
    uint32_t stx_uid;
    uint32_t stx_gid;
    uint16_t stx_mode;
    uint16_t spare0;
    uint64_t stx_ino;
    uint64_t stx_size;
    uint64_t stx_blocks;
    uint64_t stx_attributes_mask;
    uint32_t stx_atime_sec;
    uint32_t stx_atime_nsec;
    uint32_t stx_mtime_sec;
    uint32_t stx_mtime_nsec;
    uint32_t stx_ctime_sec;
    uint32_t stx_ctime_nsec;
    uint32_t stx_btime_sec;
    uint32_t stx_btime_nsec;
    uint64_t stx_rdev_major;
    uint64_t stx_rdev_minor;
    uint64_t stx_dev_major;
    uint64_t stx_dev_minor;
    uint64_t spare2[14];
};

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1

static volatile int futex_word;

static void *futex_waiter(void *arg) {
    (void)arg;
    while (futex_word == 0) {
        syscall(SYS_futex, &futex_word, FUTEX_WAIT, 1, NULL, NULL, 0);
    }
    return NULL;
}

static void bench_futex(void) {
    long iters = 100000;
    pthread_t th;
    futex_word = 0;
    pthread_create(&th, NULL, futex_waiter, NULL);
    /* let the waiter block */
    while (futex_word != 0) {}
    double t0 = now_s();
    for (long i = 0; i < iters; i++) {
        futex_word = 1;
        syscall(SYS_futex, &futex_word, FUTEX_WAKE, 1, NULL, NULL, 0);
        futex_word = 0;
    }
    pthread_join(th, NULL);
    report("futex_pingpong", iters, t0);
}

static void bench_exec_self(const char *argv0) {
    long iters = 50;
    char path[512];
    if (argv0 && argv0[0] == '/') {
        snprintf(path, sizeof(path), "%s", argv0);
    } else {
        snprintf(path, sizeof(path), "/work/bench/sysbench");
    }
    double t0 = now_s();
    for (long i = 0; i < iters; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            char *av[] = {path, "exit", NULL};
            execv(path, av);
            _exit(127);
        }
        int st;
        waitpid(pid, &st, 0);
    }
    report("fork_exec_wait", iters, t0);
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "exit") == 0) {
        return 0;
    }

    const char *file = "/work/tgoskits/Cargo.toml";
    const char *dir = "/usr/bin";
    int fd;

    /* 1. raw syscall */
    {
        long iters = 1000000;
        double t0 = now_s();
        for (long i = 0; i < iters; i++) (void)getpid();
        report("getpid", iters, t0);
    }
    {
        long iters = 1000000;
        struct timespec ts;
        double t0 = now_s();
        for (long i = 0; i < iters; i++) clock_gettime(CLOCK_MONOTONIC, &ts);
        report("clock_gettime", iters, t0);
    }

    /* 2. open/close cached */
    {
        long iters = 100000;
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            int f = open(file, O_RDONLY);
            if (f >= 0) close(f);
        }
        report("open_close_hit", iters, t0);
    }

    /* 3. open/close miss (uncached negative) */
    {
        long iters = 50000;
        char name[128];
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            snprintf(name, sizeof(name),
                     "/work/tgoskits/does-not-exist-%ld", i % 4096);
            int f = open(name, O_RDONLY);
            if (f >= 0) close(f);
        }
        report("open_close_miss", iters, t0);
    }

    /* 4. statx hit */
    {
        long iters = 100000;
        struct statx_buf st;
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            syscall(SYS_statx, AT_FDCWD, file, 0, 0x7ff, &st);
        }
        report("statx_hit", iters, t0);
    }

    /* 5. readlinkat miss (mimics rustc's failing probes) */
    {
        long iters = 50000;
        char name[128], buf[256];
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            snprintf(name, sizeof(name),
                     "/work/tgoskits/missing-link-%ld", i % 4096);
            (void)syscall(SYS_readlinkat, AT_FDCWD, name, buf, sizeof(buf));
        }
        report("readlinkat_miss", iters, t0);
    }

    /* 6. pread fixed sizes */
    {
        long iters = 100000;
        char *buf = aligned_alloc(4096, 65536);
        fd = open(file, O_RDONLY);
        double t0 = now_s();
        for (long i = 0; i < iters; i++) (void)pread(fd, buf, 4096, 0);
        report("pread_4k", iters, t0);
        close(fd);
        free(buf);
    }
    {
        long iters = 10000;
        char *buf = aligned_alloc(4096, 65536);
        fd = open(file, O_RDONLY);
        double t0 = now_s();
        for (long i = 0; i < iters; i++) (void)pread(fd, buf, 65536, 0);
        report("pread_64k", iters, t0);
        close(fd);
        free(buf);
    }

    /* 7. write 4K to page cache */
    {
        long iters = 50000;
        char *buf = aligned_alloc(4096, 4096);
        memset(buf, 0x5a, 4096);
        fd = open("/work/bench-write.tmp", O_CREAT | O_RDWR | O_TRUNC, 0644);
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            if (pwrite(fd, buf, 4096, (i % 1024) * 4096) != 4096) break;
        }
        report("pwrite_4k", iters, t0);
        close(fd);
        free(buf);
    }

    /* 8. getdents64 on a big dir */
    {
        long iters = 1000;
        char *buf = malloc(65536);
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            int d = open(dir, O_RDONLY | O_DIRECTORY);
            if (d < 0) continue;
            while (1) {
                long n = syscall(SYS_getdents64, d, buf, 65536);
                if (n <= 0) break;
            }
            close(d);
        }
        report("getdents_usrbin", iters, t0);
        free(buf);
    }

    /* 9. mmap/munmap anon 4K */
    {
        long iters = 50000;
        double t0 = now_s();
        for (long i = 0; i < iters; i++) {
            void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if (p != MAP_FAILED) munmap(p, 4096);
        }
        report("mmap_munmap_4k", iters, t0);
    }

    /* 10. mprotect 4K pages */
    {
        long iters = 50000;
        size_t len = 1024 * 4096;
        char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) {
            double t0 = now_s();
            for (long i = 0; i < iters; i++) {
                size_t off = (i % 256) * 4096;
                mprotect(p + off, 4096,
                         (i & 1) ? PROT_READ | PROT_WRITE : PROT_READ);
            }
            report("mprotect_4k", iters, t0);
            munmap(p, len);
        }
    }

    /* 11. page fault cost: touch 64MB anon */
    {
        long pages = 16384; /* 64MB / 4K */
        size_t len = pages * 4096;
        char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) {
            double t0 = now_s();
            for (long i = 0; i < pages; i++) p[i * 4096] = 1;
            report("pagefault_touch_64m", pages, t0);
            munmap(p, len);
        }
    }

    /* 12. futex ping-pong */
    bench_futex();

    /* 13. fork+exec+wait of this static binary */
    bench_exec_self(argv[0]);

    printf("sysbench done\n");
    return 0;
}
