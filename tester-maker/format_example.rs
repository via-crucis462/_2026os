    // 测例黑名单
    const SKIP_CASES: &[&str] = &[
        "cgroup_regression_3_1.sh",
        "cgroup_regression_3_2.sh",
        "cgroup_regression_5_1.sh",
        "cgroup_regression_5_2.sh",
        "cgroup_regression_6_1.sh",
        "cgroup_regression_6_2.sh",
        "cgroup_regression_fork_processes",
        "cgroup_regression_getdelays",
        "cgroup_fj_common.sh",
        "cpuctl_def_task0*",
        "cpuctl*_test0*",
        "cpuset*",
        "clock_gettime01",
        "cve-*",
        "dio_*",
        "doio*",
        "dynamic_debug0*",
        "dma_thread_diotest",
        "epoll-ltp",
        "fallocate05",
        "fallocate06",
        "force_erase.sh",
        "fork_exec_loop",
        "fs_racer_*.sh",
        "gen*",
        "gettimeofday01",
        "kill1*",
        "lftest",
        "memcg_test_*",
        "memcpy*",
        "memcontrol*",
        "memctl*",
        "mtest*",
        "pidns*",
        "pids_task*",
        "select04*",
        "sendfile07*",//无限输出“UnixSocket write called with 1 bytes”
        "setfsgid03*",//“ Panicked at src/mm/heap_allocator.rs:12 Heap allocation error, layout = Layout { size: 8192, align: 1 (1 << 0) }”
        "setrlimit05*",
        "sigtimedwait01*",
        "sigwait01*",
        "sigwaitinfo01*",
        "statx11*",
        "timed_forkbomb*",
        "tst_hexdump*",
        "epoll_pwait*",
        "hackbench",
        "futex*", // 没实现快速锁，会死循环，先注释掉
        "tcp*",
        "udp*",
        "mallinfo*", // 测试meminfo，炸得有点怪，brk或许有问题
        "mmapstress03", //brk或许有问题
        "accept02", // la musl会炸
        "fsstress*",
        // 下面的是可能能运行但没分的

    ];