
use bitflags::bitflags;

bitflags! {
    pub struct PrctlOption: usize {
        const PR_SETNAME = 1 << 15;
        const PR_GETNAME = 1 << 16;
        const PR_GET_SECCOMP = 1 << 21;
        const PR_SET_SECCOMP = 1 << 22;
        const PR_CAPBSET_READ = 1 << 23;
        const PR_GET_TSC = 1 << 25;
        const PR_SET_TSC = 1 << 26;
        const PR_SET_TIMERSLACK = 1 << 29;
        const PR_GET_TIMERSLACK = 1 << 30;
        const PR_SET_CHILD_SUBREAPER = 1 << 36;
        const PR_GET_CHILD_SUBREAPER = 1 << 37;
        const PR_SET_NO_NEW_PRIVS = 1<< 38;
        const PR_GET_NO_NEW_PRIVS = 1 << 39;
        const PR_SET_THP_DISABLE = 1 << 41;
        const PR_GET_THP_DISABLE = 1 << 42;
        const PR_CAP_AMBIENT = 1 << 47;
        const PR_GET_SPECULATION_CTRL = 1 << 52;
        const PR_SET_SPECULATION_CTRL = 1 << 53;
    }
}

bitflags! {
    pub struct AmbientOption: usize {

        const PR_CAP_AMBIENT_LOWER = 1 << 1;
        const PR_CAP_AMBIENT_IS_SET = 1 << 2;
        const PR_CAP_AMBIENT_CLEAR_ALL = 1 << 3;
    }
}

bitflags! {
    pub struct TSCOption: usize {
        const PR_TSC_ENABLE = 1 << 1;
        const PR_TSC_SIGSEGV = 1 << 2;
    }
}