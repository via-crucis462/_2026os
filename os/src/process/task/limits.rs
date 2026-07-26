#[derive(Clone, Copy)]
pub struct Rlimit { pub rlim_cur: usize, pub rlim_max: usize }
impl Rlimit {
    pub const fn new(rlim_cur: usize, rlim_max: usize) -> Self { Self { rlim_cur, rlim_max } }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rlimit64 { pub cur_lmt: usize, pub max_lmt: usize }
#[derive(Clone)]
pub struct Rlimits {
    pub cpu: Rlimit, pub fsize: Rlimit, pub data: Rlimit, pub stack: Rlimit,
    pub core: Rlimit, pub rss: Rlimit, pub nproc: Rlimit, pub nofile: Rlimit,
    pub memlock: Rlimit, pub aspace: Rlimit, pub locks: Rlimit,
    pub sigpending: Rlimit, pub msgqueue: Rlimit, pub nice: Rlimit,
    pub rtprio: Rlimit, pub rttime: Rlimit,
}
impl Rlimits {
    pub const fn new() -> Self {
        const I: usize = usize::MAX;
        const K: usize = 1024;
        const M: usize = 1024 * K;
        Self {
            cpu: Rlimit::new(I, I), fsize: Rlimit::new(I, I), data: Rlimit::new(I, I),
            stack: Rlimit::new(8 * M, I), core: Rlimit::new(0, I), rss: Rlimit::new(I, I),
            nproc: Rlimit::new(4096, 4096), nofile: Rlimit::new(1024, 4096),
            memlock: Rlimit::new(64 * K, 64 * K), aspace: Rlimit::new(I, I),
            locks: Rlimit::new(I, I), sigpending: Rlimit::new(1024, 1024),
            msgqueue: Rlimit::new(1024 * K, 1024 * K), nice: Rlimit::new(0, 0),
            rtprio: Rlimit::new(0, 0), rttime: Rlimit::new(I, I),
        }
    }
}
