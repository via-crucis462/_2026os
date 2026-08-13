#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile int failures;
static volatile int start_flag;

static double monotonic_seconds(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0.0;
    }
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static void report_case(const char *name, long operations, double elapsed) {
    double ns_per_op = operations > 0
        ? elapsed * 1000000000.0 / (double)operations
        : 0.0;
    printf("CASE %-24s ops=%ld wall=%.6f ns_op=%.1f failures=%d\n",
           name, operations, elapsed, ns_per_op, failures);
    fflush(stdout);
}

static void reset_case(void) {
    failures = 0;
    __atomic_store_n(&start_flag, 0, __ATOMIC_RELEASE);
}

static void wait_for_start(void) {
    while (__atomic_load_n(&start_flag, __ATOMIC_ACQUIRE) == 0) {
        __asm__ volatile("" ::: "memory");
    }
}

static void *thread_mmap_unmap(void *arg) {
    long loops = *(long *)arg;
    wait_for_start();
    for (long i = 0; i < loops; i++) {
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
            continue;
        }
        if (munmap(p, 4096) != 0) {
            __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
        }
    }
    return NULL;
}

struct mprotect_args {
    char *base;
    long loops;
    int worker;
    int workers;
};

static void *thread_mprotect(void *arg) {
    struct mprotect_args *a = (struct mprotect_args *)arg;
    const size_t page_size = 4096;
    const size_t pages_per_worker = 16;
    wait_for_start();
    for (long i = 0; i < a->loops; i++) {
        size_t page = ((size_t)a->worker * pages_per_worker)
                    + ((size_t)i % pages_per_worker);
        int prot = (i & 1) ? (PROT_READ | PROT_WRITE) : PROT_READ;
        if (mprotect(a->base + page * page_size, page_size, prot) != 0) {
            __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
        }
    }
    return NULL;
}

static int run_threads(const char *name, void *(*fn)(void *), void *args,
                       long loops, int workers, long operations) {
    pthread_t *threads = calloc((size_t)workers, sizeof(*threads));
    if (threads == NULL) {
        return -1;
    }
    reset_case();
    for (int i = 0; i < workers; i++) {
        if (pthread_create(&threads[i], NULL, fn, args) != 0) {
            __atomic_fetch_add(&failures, 1, __ATOMIC_RELAXED);
            workers = i;
            break;
        }
    }
    double start = monotonic_seconds();
    __atomic_store_n(&start_flag, 1, __ATOMIC_RELEASE);
    for (int i = 0; i < workers; i++) {
        (void)pthread_join(threads[i], NULL);
    }
    report_case(name, operations, monotonic_seconds() - start);
    free(threads);
    (void)loops;
    return 0;
}

static void run_seq_mmap(long loops) {
    reset_case();
    double start = monotonic_seconds();
    for (long i = 0; i < loops; i++) {
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            failures++;
            continue;
        }
        if (munmap(p, 4096) != 0) {
            failures++;
        }
    }
    report_case("mmap_unmap_4k_seq", loops,
                monotonic_seconds() - start);
}

static void run_seq_fault(long loops) {
    const size_t length = 256 * 4096;
    reset_case();
    double start = monotonic_seconds();
    for (long i = 0; i < loops; i++) {
        char *p = mmap(NULL, length, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            failures++;
            continue;
        }
        for (size_t off = 0; off < length; off += 4096) {
            p[off] = (char)(off >> 12);
        }
        if (munmap(p, length) != 0) {
            failures++;
        }
    }
    report_case("mmap_fault_1m_seq", loops * (long)(length / 4096),
                monotonic_seconds() - start);
}

static void run_seq_mprotect(long loops) {
    const size_t length = 256 * 4096;
    char *p = mmap(NULL, length, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) {
        failures++;
        report_case("mprotect_4k_seq", 0, 0.0);
        return;
    }
    reset_case();
    double start = monotonic_seconds();
    for (long i = 0; i < loops; i++) {
        size_t off = ((size_t)i % (length / 4096)) * 4096;
        int prot = (i & 1) ? (PROT_READ | PROT_WRITE) : PROT_READ;
        if (mprotect(p + off, 4096, prot) != 0) {
            failures++;
        }
    }
    report_case("mprotect_4k_seq", loops,
                monotonic_seconds() - start);
    (void)munmap(p, length);
}

static void run_process_mmap(long loops, int workers) {
    reset_case();
    double start = monotonic_seconds();
    int created = 0;
    for (int i = 0; i < workers; i++) {
        pid_t pid = fork();
        if (pid < 0) {
            failures++;
            continue;
        }
        if (pid == 0) {
            for (long j = 0; j < loops; j++) {
                void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
                if (p == MAP_FAILED) _exit(2);
                if (munmap(p, 4096) != 0) _exit(3);
            }
            _exit(0);
        }
        created++;
    }
    for (int i = 0; i < created; i++) {
        int status;
        if (wait(&status) < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            failures++;
        }
    }
    report_case("mmap_unmap_4k_proc", (long)created * loops,
                monotonic_seconds() - start);
}

static int run_all(int argc, char **argv) {
    long loops = argc > 1 ? atol(argv[1]) : 5000;
    int workers = argc > 2 ? atoi(argv[2]) : 8;
    if (loops < 1) loops = 1;
    if (workers < 1) workers = 1;
    if (workers > 32) workers = 32;

    printf("MMBENCH_BEGIN loops=%ld workers=%d\n", loops, workers);
    run_seq_mmap(loops * 10);
    run_seq_fault(loops / 10 + 1);
    run_seq_mprotect(loops * 10);

    long mmap_ops = loops * 10;
    run_threads("mmap_unmap_4k_thr", thread_mmap_unmap, &mmap_ops,
                mmap_ops, workers, mmap_ops * workers);

    size_t pages = (size_t)workers * 16;
    char *shared = mmap(NULL, pages * 4096, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) {
        failures++;
        report_case("mprotect_4k_thr", 0, 0.0);
    } else {
        struct mprotect_args *args = calloc((size_t)workers, sizeof(*args));
        pthread_t *threads = calloc((size_t)workers, sizeof(*threads));
        reset_case();
        double start = monotonic_seconds();
        int created = 0;
        if (args != NULL && threads != NULL) {
            for (int i = 0; i < workers; i++) {
                args[i] = (struct mprotect_args){shared, loops, i, workers};
                if (pthread_create(&threads[i], NULL, thread_mprotect, &args[i]) != 0) {
                    failures++;
                    break;
                }
                created++;
            }
            __atomic_store_n(&start_flag, 1, __ATOMIC_RELEASE);
            for (int i = 0; i < created; i++) (void)pthread_join(threads[i], NULL);
        } else {
            failures++;
        }
        report_case("mprotect_4k_thr", (long)created * loops,
                    monotonic_seconds() - start);
        free(threads);
        free(args);
        (void)munmap(shared, pages * 4096);
    }

    run_process_mmap(mmap_ops, workers);
    return failures ? 1 : 0;
}

int main(int argc, char **argv) {
    pid_t pid = fork();
    if (pid < 0) {
        printf("MMBENCH_END fork_failed errno=%d\n", errno);
        return 1;
    }
    if (pid == 0) {
        _exit(run_all(argc, argv));
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        printf("MMBENCH_END wait_failed errno=%d\n", errno);
        return 1;
    }
    int failed = !WIFEXITED(status) || WEXITSTATUS(status) != 0;
    printf("MMBENCH_END status=%d failures=%d\n", status, failed);
    return failed;
}
