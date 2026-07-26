#[derive(Copy, Clone, PartialEq , Debug)]
/// task status: UnInit, Ready, Running, Exited
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// 被阻塞（目前是被锁阻塞）
    Blocked,
    /// exited
    Zombie,
    /// 加入了等待队列但正在保存上下文
    BlockSaving,
}

impl core::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let status = match self {
            TaskStatus::UnInit => "UnInit",
            TaskStatus::Ready => "Ready",
            TaskStatus::Running => "Running",
            TaskStatus::Blocked => "Blocked",
            TaskStatus::Zombie => "Zombie",
            TaskStatus::BlockSaving => "BlockSaving",
        };
        f.write_str(status)
    }
}
