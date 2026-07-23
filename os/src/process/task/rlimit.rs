#[derive(Clone, Copy)]
pub struct Rlimit {
    pub rlim_cur: usize,
    pub rlim_max: usize,
}

impl Rlimit {
    pub const fn new(rlim_cur: usize, rlim_max: usize) -> Self {
        Self { rlim_cur, rlim_max }
    }
}

#[derive(Clone)]
pub struct Rlimits {
    pub cpu: Rlimit,
    pub fsize: Rlimit,
    pub data: Rlimit,
    pub stack: Rlimit,
    pub core: Rlimit,
    pub rss: Rlimit,
    pub nproc: Rlimit,
    pub nofile: Rlimit,
    pub memlock: Rlimit,
    pub aspace: Rlimit,
    pub locks: Rlimit,
    pub sigpending: Rlimit,
    pub msgqueue: Rlimit,
    pub nice: Rlimit,
    pub rtprio: Rlimit,
    pub rttime: Rlimit,
}

impl Rlimits {
    pub const fn new() -> Self {
        const INFINITY: usize = usize::MAX;
        const KIB: usize = 1024;
        const MIB: usize = 1024 * KIB;
        Self {
            cpu: Rlimit::new(INFINITY, INFINITY),
            fsize: Rlimit::new(INFINITY, INFINITY),
            data: Rlimit::new(INFINITY, INFINITY),
            stack: Rlimit::new(8 * MIB, INFINITY),
            core: Rlimit::new(0, INFINITY),
            rss: Rlimit::new(INFINITY, INFINITY),
            nproc: Rlimit::new(4096, 4096),
            nofile: Rlimit::new(1024, 4096),
            memlock: Rlimit::new(64 * KIB, 64 * KIB),
            aspace: Rlimit::new(INFINITY, INFINITY),
            locks: Rlimit::new(INFINITY, INFINITY),
            sigpending: Rlimit::new(1024, 1024),
            msgqueue: Rlimit::new(1024 * KIB, 1024 * KIB),
            nice: Rlimit::new(0, 0),
            rtprio: Rlimit::new(0, 0),
            rttime: Rlimit::new(INFINITY, INFINITY),
        }
    }
}