use super::task::*;

pub struct Process {
    pub pid: PID,
    pub parent: Option<PID>,
    pub inner: ProcessInner,
}