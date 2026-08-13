#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_clock_gettime
#define SYS_clock_gettime 113
#endif

static inline uint64_t rdtime(void) {
    uint64_t t;
    __asm__ volatile("rdtime %0" : "=r"(t));
    return t;
}

static int clock_ns(int clk, int64_t *sec, int64_t *nsec) {
    struct timespec ts;
    memset(&ts, 0, sizeof(ts));
    int rc = (int)syscall(SYS_clock_gettime, clk, &ts);
    if (rc == 0) {
        *sec = (int64_t)ts.tv_sec;
        *nsec = (int64_t)ts.tv_nsec;
        if (ts.tv_nsec < 0 || ts.tv_nsec >= 1000000000LL || ts.tv_sec < 0) {
            printf("BAD_CLOCK clk=%d sec=%lld nsec=%lld\n", clk,
                   (long long)ts.tv_sec, (long long)ts.tv_nsec);
            return -1;
        }
    } else {
        printf("CLOCK_GETTIME_FAIL clk=%d errno=%d\n", clk, errno);
    }
    return rc;
}

static double now_s_clk(int clk) {
    int64_t s, ns;
    if (clock_ns(clk, &s, &ns) == 0) {
        return (double)s + (double)ns / 1000000000.0;
    }
    return 0.0;
}

struct thread_ctx {
    int cpu;
    int64_t sec0, ns0;
    uint64_t rdt0;
    int64_t min_delta_ns;
    int64_t max_delta_ns;
    int backwards;
    int bad;
    long iters;
};

static void *clock_worker(void *arg) {
    struct thread_ctx *c = (struct thread_ctx *)arg;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(c->cpu, &set);
    sched_setaffinity(0, sizeof(set), &set);

    /* warm up */
    for (int i = 0; i < 1000; i++) {
        int64_t s, ns;
        clock_ns(CLOCK_MONOTONIC, &s, &ns);
    }
    int64_t s, ns;
    clock_ns(CLOCK_MONOTONIC, &s, &ns);
    c->sec0 = s;
    c->ns0 = ns;
    c->rdt0 = rdtime();

    int64_t prev_s = s, prev_ns = ns;
    int64_t min_delta = INT64_MAX, max_delta = 0;
    int backwards = 0, bad = 0;
    long iters = 0;
    for (iters = 0; iters < c->iters; iters++) {
        int64_t cur_s, cur_ns;
        if (clock_ns(CLOCK_MONOTONIC, &cur_s, &cur_ns) != 0) {
            bad++;
            continue;
        }
        int64_t delta =
            (cur_s - prev_s) * 1000000000LL + (cur_ns - prev_ns);
        if (delta < 0) backwards++;
        if (delta < min_delta) min_delta = delta;
        if (delta > max_delta) max_delta = delta;
        prev_s = cur_s;
        prev_ns = cur_ns;
    }
    c->min_delta_ns = min_delta;
    c->max_delta_ns = max_delta;
    c->backwards = backwards;
    c->bad = bad;
    return NULL;
}

struct migrate_ctx {
    int id;
    int64_t min_delta_ns;
    int64_t max_delta_ns;
    int backwards;
    long iters;
};

static void *migrate_worker(void *arg) {
    struct migrate_ctx *c = (struct migrate_ctx *)arg;
    /* 不设置 affinity，让调度器自由迁移线程，暴露 per-hart 时钟漂移 */
    int64_t s, ns;
    clock_ns(CLOCK_MONOTONIC, &s, &ns);
    int64_t ps = s, pn = ns;
    int64_t min_delta = INT64_MAX, max_delta = 0;
    int backwards = 0;
    for (long i = 0; i < c->iters; i++) {
        clock_ns(CLOCK_MONOTONIC, &s, &ns);
        int64_t d = (s - ps) * 1000000000LL + (ns - pn);
        if (d < 0) backwards++;
        if (d < min_delta) min_delta = d;
        if (d > max_delta) max_delta = d;
        ps = s;
        pn = ns;
        /* 小忙等提高被迁移概率 */
        for (volatile int k = 0; k < 100; k++) {}
    }
    c->min_delta_ns = min_delta;
    c->max_delta_ns = max_delta;
    c->backwards = backwards;
    return NULL;
}

int main(int argc, char **argv) {
    long iters = argc > 1 ? atol(argv[1]) : 200000;
    int nthreads = argc > 2 ? atoi(argv[2]) : 8;
    int nprocs = argc > 3 ? atoi(argv[3]) : 0;
    if (nthreads > 64) nthreads = 64;
    if (nprocs > 64) nprocs = 64;

    printf("timeprobe begin iters=%ld threads=%d procs=%d\n", iters, nthreads, nprocs);

    /* Sequential sanity: monotonic should advance ~100ms over usleep(100000) */
    double t0 = now_s_clk(CLOCK_MONOTONIC);
    struct timespec req = {0, 100000000};
    nanosleep(&req, NULL);
    double t1 = now_s_clk(CLOCK_MONOTONIC);
    printf("sleep100ms_mono_delta=%.6f s\n", t1 - t0);

    /* Sequential monotonic delta distribution */
    int64_t s0, ns0, s1, ns1;
    clock_ns(CLOCK_MONOTONIC, &s0, &ns0);
    int64_t min_gap = INT64_MAX, max_gap = 0, neg = 0;
    int64_t ps = s0, pn = ns0;
    for (long i = 0; i < 100000; i++) {
        int64_t cs, cn;
        clock_ns(CLOCK_MONOTONIC, &cs, &cn);
        int64_t d = (cs - ps) * 1000000000LL + (cn - pn);
        if (d < 0) neg++;
        if (d < min_gap) min_gap = d;
        if (d > max_gap) max_gap = d;
        ps = cs;
        pn = cn;
    }
    printf("seq_min_gap_ns=%lld max_gap_ns=%lld backwards=%lld\n",
           (long long)min_gap, (long long)max_gap, (long long)neg);

    /* Realtime vs monotonic */
    int64_t rs, rns, ms, mns;
    clock_ns(CLOCK_REALTIME, &rs, &rns);
    clock_ns(CLOCK_MONOTONIC, &ms, &mns);
    printf("realtime_sec=%lld realtime_nsec=%lld mono_sec=%lld mono_nsec=%lld\n",
           (long long)rs, (long long)rns, (long long)ms, (long long)mns);

    /* Threads pinned to CPUs */
    pthread_t th[64];
    struct thread_ctx ctx[64];
    for (int i = 0; i < nthreads; i++) {
        ctx[i].cpu = i;
        ctx[i].iters = iters;
        ctx[i].backwards = -1;
        ctx[i].bad = 0;
        pthread_create(&th[i], NULL, clock_worker, &ctx[i]);
    }
    for (int i = 0; i < nthreads; i++) {
        pthread_join(th[i], NULL);
        printf("cpu%d mono0=%lld.%09lld rdt0=%llu min_gap=%lld max_gap=%lld backwards=%d bad=%d\n",
               i,
               (long long)ctx[i].sec0, (long long)ctx[i].ns0,
               (unsigned long long)ctx[i].rdt0,
               (long long)ctx[i].min_delta_ns,
               (long long)ctx[i].max_delta_ns,
               ctx[i].backwards, ctx[i].bad);
    }

    /* 不钉核的线程：线程在 hart 间迁移，检验全局单调性 */
    struct migrate_ctx mc[8];
    int mn = nthreads < 8 ? nthreads : 8;
    for (int i = 0; i < mn; i++) {
        mc[i].id = i;
        mc[i].iters = iters;
        pthread_create(&th[i], NULL, migrate_worker, &mc[i]);
    }
    for (int i = 0; i < mn; i++) {
        pthread_join(th[i], NULL);
        printf("mig%d min_gap=%lld max_gap=%lld backwards=%d\n",
               i, (long long)mc[i].min_delta_ns,
               (long long)mc[i].max_delta_ns, mc[i].backwards);
    }

    printf("timeprobe end\n");
    return 0;
}
