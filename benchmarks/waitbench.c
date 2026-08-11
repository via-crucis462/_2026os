#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_futex
#define SYS_futex 98
#endif
#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif
#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif

#define NS_PER_S 1000000000ULL

static long iters = 2000;

static double monotonic_seconds(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(2);
    }
    return (double)ts.tv_sec + (double)ts.tv_nsec / NS_PER_S;
}

static void report(const char *name, double start, long operations) {
    double elapsed = monotonic_seconds() - start;
    printf("WAITBENCH %-20s iters=%ld wall=%.6f s ns/op=%.1f\n",
           name, operations, elapsed,
           elapsed * (double)NS_PER_S / (double)operations);
    fflush(stdout);
}

static void fail_errno(const char *what) {
    fprintf(stderr, "WAITBENCH_FAIL %s errno=%d (%s)\n",
            what, errno, strerror(errno));
    exit(1);
}

static void write_u64(int fd, uint64_t value) {
    for (;;) {
        ssize_t n = write(fd, &value, sizeof(value));
        if (n == (ssize_t)sizeof(value)) return;
        if (n < 0 && errno == EINTR) continue;
        fail_errno("write(eventfd)");
    }
}

static uint64_t read_u64(int fd) {
    uint64_t value;
    for (;;) {
        ssize_t n = read(fd, &value, sizeof(value));
        if (n == (ssize_t)sizeof(value)) return value;
        if (n < 0 && errno == EINTR) continue;
        fail_errno("read(eventfd)");
    }
}

struct event_pair {
    int data_fd;
    int ack_fd;
    long count;
};

static void *event_writer(void *arg) {
    struct event_pair *ctx = arg;
    for (long i = 0; i < ctx->count; i++) {
        write_u64(ctx->data_fd, 1);
        (void)read_u64(ctx->ack_fd);
    }
    return NULL;
}

static void wait_eventfd_direct(int fd) {
    (void)fd;
}

static void wait_eventfd_epoll(int epfd) {
    struct epoll_event event;
    for (;;) {
        int n = epoll_wait(epfd, &event, 1, -1);
        if (n == 1) break;
        if (n < 0 && errno == EINTR) continue;
        fail_errno("epoll_wait");
    }
    if ((event.events & (EPOLLIN | EPOLLERR | EPOLLHUP)) == 0) {
        fprintf(stderr, "WAITBENCH_FAIL epoll events=0x%x\n", event.events);
        exit(1);
    }
}

static void wait_eventfd_ppoll(int fd) {
    struct pollfd pfd = { .fd = fd, .events = POLLIN, .revents = 0 };
    for (;;) {
        int n = ppoll(&pfd, 1, NULL, NULL);
        if (n == 1) break;
        if (n < 0 && errno == EINTR) continue;
        fail_errno("ppoll");
    }
    if ((pfd.revents & (POLLIN | POLLERR | POLLHUP)) == 0) {
        fprintf(stderr, "WAITBENCH_FAIL ppoll revents=0x%x\n", pfd.revents);
        exit(1);
    }
}

static void bench_event_path(const char *name, int use_epoll, int use_ppoll) {
    int data_fd = eventfd(0, 0);
    int ack_fd = eventfd(0, 0);
    if (data_fd < 0 || ack_fd < 0) fail_errno("eventfd");

    int epfd = -1;
    if (use_epoll) {
        epfd = epoll_create1(0);
        if (epfd < 0) fail_errno("epoll_create1");
        struct epoll_event event = { .events = EPOLLIN, .data.fd = data_fd };
        if (epoll_ctl(epfd, EPOLL_CTL_ADD, data_fd, &event) != 0) {
            fail_errno("epoll_ctl");
        }
    }

    struct event_pair ctx = { .data_fd = data_fd, .ack_fd = ack_fd, .count = iters };
    pthread_t writer;
    if (pthread_create(&writer, NULL, event_writer, &ctx) != 0) {
        fail_errno("pthread_create");
    }

    double start = monotonic_seconds();
    for (long i = 0; i < iters; i++) {
        if (use_epoll) wait_eventfd_epoll(epfd);
        else if (use_ppoll) wait_eventfd_ppoll(data_fd);
        else wait_eventfd_direct(data_fd);
        (void)read_u64(data_fd);
        write_u64(ack_fd, 1);
    }
    if (pthread_join(writer, NULL) != 0) fail_errno("pthread_join");
    report(name, start, iters);

    close(data_fd);
    close(ack_fd);
    if (epfd >= 0) close(epfd);
}

struct pipe_pair {
    int data_read;
    int data_write;
    int ack_read;
    int ack_write;
    long count;
};

static void *pipe_writer(void *arg) {
    struct pipe_pair *ctx = arg;
    char byte = 'x';
    for (long i = 0; i < ctx->count; i++) {
        while (write(ctx->data_write, &byte, 1) != 1) {
            if (errno != EINTR) fail_errno("write(pipe)");
        }
        while (read(ctx->ack_read, &byte, 1) != 1) {
            if (errno != EINTR) fail_errno("read(pipe ack)");
        }
    }
    return NULL;
}

static void bench_pipe(const char *name, int use_epoll) {
    int data[2], ack[2];
    if (pipe(data) != 0 || pipe(ack) != 0) fail_errno("pipe");

    int epfd = -1;
    if (use_epoll) {
        epfd = epoll_create1(0);
        if (epfd < 0) fail_errno("epoll_create1 pipe");
        struct epoll_event event = { .events = EPOLLIN, .data.fd = data[0] };
        if (epoll_ctl(epfd, EPOLL_CTL_ADD, data[0], &event) != 0) {
            fail_errno("epoll_ctl pipe");
        }
    }

    struct pipe_pair ctx = {
        .data_read = data[0], .data_write = data[1],
        .ack_read = ack[0], .ack_write = ack[1], .count = iters,
    };
    pthread_t writer;
    if (pthread_create(&writer, NULL, pipe_writer, &ctx) != 0) {
        fail_errno("pthread_create");
    }

    double start = monotonic_seconds();
    for (long i = 0; i < iters; i++) {
        if (use_epoll) {
            struct epoll_event event;
            int n;
            do n = epoll_wait(epfd, &event, 1, -1); while (n < 0 && errno == EINTR);
            if (n != 1 || (event.events & EPOLLIN) == 0) fail_errno("epoll_wait(pipe)");
        } else {
            struct pollfd pfd = { .fd = data[0], .events = POLLIN, .revents = 0 };
            int n;
            do n = ppoll(&pfd, 1, NULL, NULL); while (n < 0 && errno == EINTR);
            if (n != 1 || (pfd.revents & POLLIN) == 0) fail_errno("ppoll(pipe)");
        }
        char byte;
        if (read(data[0], &byte, 1) != 1) fail_errno("read(pipe)");
        if (write(ack[1], &byte, 1) != 1) fail_errno("write(pipe ack)");
    }
    if (pthread_join(writer, NULL) != 0) fail_errno("pthread_join");
    report(name, start, iters);
    close(data[0]); close(data[1]); close(ack[0]); close(ack[1]);
    if (epfd >= 0) close(epfd);
}

static void bench_ppoll_pipe(void) {
    bench_pipe("ppoll_pipe", 0);
}

static void bench_epoll_pipe(void) {
    bench_pipe("epoll_pipe", 1);
}

static void bench_timeout(const char *name, int use_epoll, int timeout_ms, long max_rounds) {
    int fd = -1, hold_fd = -1, epfd = -1;
    if (use_epoll) {
        epfd = epoll_create1(0);
        if (epfd < 0) fail_errno("epoll_create1 timeout");
    } else {
        int p[2];
        if (pipe(p) != 0) fail_errno("pipe timeout");
        fd = p[0];
        hold_fd = p[1];
    }

    struct timespec timeout = {
        .tv_sec = timeout_ms / 1000,
        .tv_nsec = (timeout_ms % 1000) * 1000000L,
    };
    long rounds = iters < max_rounds ? iters : max_rounds;
    double start = monotonic_seconds();
    for (long i = 0; i < rounds; i++) {
        int n;
        if (use_epoll) {
            struct epoll_event event;
            n = epoll_wait(epfd, &event, 1, timeout_ms);
        } else {
            struct pollfd pfd = { .fd = fd, .events = POLLIN, .revents = 0 };
            n = ppoll(&pfd, 1, &timeout, NULL);
        }
        if (n != 0) {
            fprintf(stderr, "WAITBENCH_FAIL %s expected timeout got=%d\n", name, n);
            exit(1);
        }
    }
    report(name, start, rounds);
    if (fd >= 0) close(fd);
    if (hold_fd >= 0) close(hold_fd);
    if (epfd >= 0) close(epfd);
}

struct futex_pair {
    volatile int data;
    volatile int ack;
    long count;
};

static int futex_op(volatile int *word, int operation, int value) {
    return (int)syscall(SYS_futex, word, operation, value, NULL, NULL, 0);
}

static void futex_wait_zero(volatile int *word) {
    while (__atomic_load_n(word, __ATOMIC_ACQUIRE) == 0) {
        int rc = futex_op(word, FUTEX_WAIT, 0);
        if (rc < 0 && errno != EAGAIN && errno != EINTR) fail_errno("futex wait");
    }
}

static void *futex_reader(void *arg) {
    struct futex_pair *ctx = arg;
    for (long i = 0; i < ctx->count; i++) {
        futex_wait_zero(&ctx->data);
        __atomic_store_n(&ctx->data, 0, __ATOMIC_RELEASE);
        futex_op(&ctx->data, FUTEX_WAKE, 1);
        __atomic_store_n(&ctx->ack, 1, __ATOMIC_RELEASE);
        futex_op(&ctx->ack, FUTEX_WAKE, 1);
        while (__atomic_load_n(&ctx->ack, __ATOMIC_ACQUIRE) != 0) {
            futex_op(&ctx->ack, FUTEX_WAIT, 1);
        }
    }
    return NULL;
}

static void bench_futex(void) {
    struct futex_pair ctx = { .data = 0, .ack = 0, .count = iters };
    pthread_t reader;
    if (pthread_create(&reader, NULL, futex_reader, &ctx) != 0) fail_errno("pthread_create");

    double start = monotonic_seconds();
    for (long i = 0; i < iters; i++) {
        __atomic_store_n(&ctx.data, 1, __ATOMIC_RELEASE);
        futex_op(&ctx.data, FUTEX_WAKE, 1);
        while (__atomic_load_n(&ctx.ack, __ATOMIC_ACQUIRE) == 0) {
            futex_op(&ctx.ack, FUTEX_WAIT, 0);
        }
        __atomic_store_n(&ctx.ack, 0, __ATOMIC_RELEASE);
        futex_op(&ctx.ack, FUTEX_WAKE, 1);
    }
    if (pthread_join(reader, NULL) != 0) fail_errno("pthread_join");
    report("futex_strict", start, iters);
}

static void phase_eventfd_direct(void) {
    bench_event_path("eventfd_direct", 0, 0);
}

static void phase_epoll_eventfd(void) {
    bench_event_path("epoll_eventfd", 1, 0);
}

static void phase_ppoll_eventfd(void) {
    bench_event_path("ppoll_eventfd", 0, 1);
}

static void phase_ppoll_timeout(void) {
    bench_timeout("ppoll_timeout_1ms", 0, 1, 1000);
}

static void phase_epoll_timeout(void) {
    bench_timeout("epoll_timeout_1ms", 1, 1, 1000);
}

static void phase_ppoll_timeout_5ms(void) {
    bench_timeout("ppoll_timeout_5ms", 0, 5, 100);
}

static void phase_epoll_timeout_5ms(void) {
    bench_timeout("epoll_timeout_5ms", 1, 5, 100);
}

static void phase_ppoll_timeout_10ms(void) {
    bench_timeout("ppoll_timeout_10ms", 0, 10, 100);
}

static void phase_epoll_timeout_10ms(void) {
    bench_timeout("epoll_timeout_10ms", 1, 10, 100);
}

static int run_phase(const char *name, void (*phase)(void)) {
    pid_t pid = fork();
    if (pid < 0) fail_errno("fork");
    if (pid == 0) {
        phase();
        _exit(0);
    }

    int status;
    if (waitpid(pid, &status, 0) != pid) fail_errno("waitpid");
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) return 0;

    printf("WAITBENCH_PHASE_FAIL %-15s status=%d\n", name, status);
    fflush(stdout);
    return 1;
}

int main(int argc, char **argv) {
    if (argc > 1) iters = atol(argv[1]);
    if (iters < 1) iters = 1;
    if (iters > 1000000) iters = 1000000;
    printf("WAITBENCH_BEGIN iters=%ld\n", iters);
    fflush(stdout);

    int failures = 0;
    failures += run_phase("futex_strict", bench_futex);
    failures += run_phase("eventfd_direct", phase_eventfd_direct);
    failures += run_phase("epoll_eventfd", phase_epoll_eventfd);
    failures += run_phase("ppoll_eventfd", phase_ppoll_eventfd);
    failures += run_phase("ppoll_pipe", bench_ppoll_pipe);
    failures += run_phase("epoll_pipe", bench_epoll_pipe);
    failures += run_phase("ppoll_timeout", phase_ppoll_timeout);
    failures += run_phase("epoll_timeout", phase_epoll_timeout);
    failures += run_phase("ppoll_timeout_5ms", phase_ppoll_timeout_5ms);
    failures += run_phase("epoll_timeout_5ms", phase_epoll_timeout_5ms);
    failures += run_phase("ppoll_timeout_10ms", phase_ppoll_timeout_10ms);
    failures += run_phase("epoll_timeout_10ms", phase_epoll_timeout_10ms);
    printf("WAITBENCH_DONE phase_failures=%d\n", failures);
    return 0;
}
