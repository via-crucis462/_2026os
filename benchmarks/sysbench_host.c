#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

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
#ifndef SYS_brk
#define SYS_brk 214
#endif

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1

static void phase(const char *name) {
    printf("====PHASE_%s_START====\n", name);
    fflush(stdout);
}

static void phase_end(const char *name) {
    printf("====PHASE_%s_END====\n", name);
    fflush(stdout);
}

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static volatile unsigned long sink;

int main(void) {
    const char *file = "/work/tgoskits/Cargo.toml";
    const char *dir = "/usr/bin";
    char *buf4k = aligned_alloc(4096, 65536);
    char *buf64k = aligned_alloc(4096, 65536);
    char name[256];
    unsigned long i;

    phase("getpid_1M");
    for (i = 0; i < 1000000UL; i++) sink += (unsigned long)getpid();
    phase_end("getpid_1M");

    phase("clock_gettime_500K");
    for (i = 0; i < 500000UL; i++) {
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        sink += ts.tv_nsec;
    }
    phase_end("clock_gettime_500K");

    phase("gettimeofday_500K");
    for (i = 0; i < 500000UL; i++) {
        struct timeval tv;
        gettimeofday(&tv, 0);
        sink += tv.tv_usec;
    }
    phase_end("gettimeofday_500K");

    phase("open_close_hit_20K");
    for (i = 0; i < 20000UL; i++) {
        int fd = open(file, O_RDONLY);
        if (fd >= 0) close(fd);
    }
    phase_end("open_close_hit_20K");

    phase("open_close_miss_10K");
    for (i = 0; i < 10000UL; i++) {
        snprintf(name, sizeof(name), "/work/tgoskits/missing-%lu", i % 4096);
        int fd = open(name, O_RDONLY);
        if (fd >= 0) close(fd);
    }
    phase_end("open_close_miss_10K");

    phase("statx_20K");
    {
        char st[256];
        for (i = 0; i < 20000UL; i++)
            syscall(SYS_statx, AT_FDCWD, file, 0, 0x7ff, st);
    }
    phase_end("statx_20K");

    phase("fstat_20K");
    {
        int fd = open(file, O_RDONLY);
        struct stat st;
        for (i = 0; i < 20000UL; i++) fstat(fd, &st);
        close(fd);
    }
    phase_end("fstat_20K");

    phase("readlinkat_miss_10K");
    for (i = 0; i < 10000UL; i++) {
        snprintf(name, sizeof(name), "/work/tgoskits/miss-link-%lu", i % 4096);
        syscall(SYS_readlinkat, AT_FDCWD, name, buf4k, 256);
    }
    phase_end("readlinkat_miss_10K");

    phase("pread_4K_20K");
    {
        int fd = open(file, O_RDONLY);
        for (i = 0; i < 20000UL; i++) pread(fd, buf4k, 4096, 0);
        close(fd);
    }
    phase_end("pread_4K_20K");

    phase("pread_64K_2K");
    {
        int fd = open(file, O_RDONLY);
        for (i = 0; i < 2000UL; i++) pread(fd, buf64k, 65536, 0);
        close(fd);
    }
    phase_end("pread_64K_2K");

    phase("mmap_munmap_20K");
    for (i = 0; i < 20000UL; i++) {
        void *p = mmap(0, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p != MAP_FAILED) munmap(p, 4096);
    }
    phase_end("mmap_munmap_20K");

    phase("mprotect_20K");
    {
        char *p = mmap(0, 4096 * 256, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        for (i = 0; i < 20000UL; i++) {
            size_t off = (i % 256) * 4096;
            mprotect(p + off, 4096,
                     (i & 1) ? PROT_READ | PROT_WRITE : PROT_READ);
        }
        munmap(p, 4096 * 256);
    }
    phase_end("mprotect_20K");

    phase("brk_50K");
    {
        unsigned long cur = (unsigned long)syscall(SYS_brk, 0);
        for (i = 0; i < 50000UL; i++) {
            syscall(SYS_brk, cur);
            syscall(SYS_brk, cur + 8192);
        }
        syscall(SYS_brk, cur);
    }
    phase_end("brk_50K");

    phase("getdents_usrbin_500");
    {
        char *dbuf = malloc(65536);
        for (i = 0; i < 500UL; i++) {
            int fd = open(dir, O_RDONLY | O_DIRECTORY);
            if (fd < 0) continue;
            while (1) {
                long n = syscall(SYS_getdents64, fd, dbuf, 65536);
                if (n <= 0) break;
            }
            close(fd);
        }
        free(dbuf);
    }
    phase_end("getdents_usrbin_500");

    phase("pipe_pingpong_20K");
    {
        int fds[2];
        char c = 'x';
        pipe(fds);
        for (i = 0; i < 20000UL; i++) {
            write(fds[1], &c, 1);
            read(fds[0], &c, 1);
        }
        close(fds[0]);
        close(fds[1]);
    }
    phase_end("pipe_pingpong_20K");

    phase("fork_wait_100");
    for (i = 0; i < 100UL; i++) {
        pid_t pid = fork();
        if (pid == 0) _exit(0);
        if (pid > 0) waitpid(pid, 0, 0);
    }
    phase_end("fork_wait_100");

    free(buf4k);
    free(buf64k);
    printf("SYSBENCH_DONE sink=%lu\n", sink);
    return 0;
}
