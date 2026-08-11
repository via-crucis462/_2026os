#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <unistd.h>

static int ITERS = 2000;

static void phase(const char *name) {
    printf("====PHASE_%s_START====\n", name);
    fflush(stdout);
}

static void phase_end(const char *name) {
    printf("====PHASE_%s_END====\n", name);
    fflush(stdout);
}

static void *event_writer(void *arg) {
    int fd = (int)(intptr_t)arg;
    uint64_t one = 1;
    for (int i = 0; i < ITERS; i++) {
        if (write(fd, &one, 8) != 8) break;
    }
    return 0;
}

static void bench_eventfd(void) {
    int fd = eventfd(0, 0);
    pthread_t th;
    pthread_create(&th, 0, event_writer, (void *)(intptr_t)fd);
    for (int i = 0; i < ITERS; i++) {
        uint64_t v;
        if (read(fd, &v, 8) != 8) break;
    }
    pthread_join(th, 0);
    close(fd);
}

static void bench_epoll(void) {
    int efd = eventfd(0, 0);
    int ep = epoll_create1(0);
    struct epoll_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.events = EPOLLIN;
    ev.data.fd = efd;
    epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ev);
    pthread_t th;
    pthread_create(&th, 0, event_writer, (void *)(intptr_t)efd);
    for (int i = 0; i < ITERS; i++) {
        struct epoll_event out;
        if (epoll_wait(ep, &out, 1, -1) <= 0) break;
        uint64_t v;
        if (read(efd, &v, 8) != 8) break;
    }
    pthread_join(th, 0);
    close(efd);
    close(ep);
}

static void *pipe_writer(void *arg) {
    int fd = (int)(intptr_t)arg;
    char c = 'x';
    for (int i = 0; i < ITERS; i++) {
        if (write(fd, &c, 1) != 1) break;
    }
    return 0;
}

static void bench_ppoll(void) {
    int fds[2];
    pipe(fds);
    pthread_t th;
    pthread_create(&th, 0, pipe_writer, (void *)(intptr_t)fds[1]);
    for (int i = 0; i < ITERS; i++) {
        struct pollfd p;
        p.fd = fds[0];
        p.events = POLLIN;
        p.revents = 0;
        int r = ppoll(&p, 1, 0, 0);
        if (r <= 0) break;
        char c;
        if (read(fds[0], &c, 1) != 1) break;
    }
    pthread_join(th, 0);
    close(fds[0]);
    close(fds[1]);
}

int main(int argc, char **argv) {
    if (argc > 1) ITERS = atoi(argv[1]);
    if (ITERS <= 0) ITERS = 2000;

    phase("eventfd_pingpong");
    bench_eventfd();
    phase_end("eventfd_pingpong");

    phase("epoll_eventfd");
    bench_epoll();
    phase_end("epoll_eventfd");

    phase("ppoll_pipe");
    bench_ppoll();
    phase_end("ppoll_pipe");

    printf("EVENTBENCH_DONE iters=%d\n", ITERS);
    return 0;
}
