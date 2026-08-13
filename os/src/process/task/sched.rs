//! 任务调度实体定义。
//!
//! 保存嵌入 `TaskStructInner` 的 CFS、实时和 Deadline 调度状态，
//! 具体运行队列及选择算法仍位于 `process::scheduler`。

/// CFS 调度实体，对应 Linux `struct sched_entity` 的基础运行时字段。
#[derive(Clone, Copy, Debug)]
pub struct SchedEntity {
    /// MLFQ 层级：0、1、2 分别对应时间片 1、2、5 ms。
    pub queue_level: usize,
    /// 当前实体的归一化虚拟运行时间。
    pub vruntime: u64,
    /// 最近一次被调度到 CPU 时的调度时钟。
    pub exec_start: u64,
    /// 实体累计执行时间。
    pub sum_exec_runtime: u64,
    /// 上一次调度统计时记录的累计执行时间。
    pub prev_sum_exec_runtime: u64,
    /// CFS 负载权重；普通 nice=0 任务默认使用 1024。
    pub load_weight: u64,
}

impl SchedEntity {
    /// 创建 nice=0、尚未运行的 CFS 调度实体。
    pub const fn new() -> Self {
        Self {
            queue_level: 0,
            vruntime: 0,
            exec_start: 0,
            sum_exec_runtime: 0,
            prev_sum_exec_runtime: 0,
            load_weight: 1024,
        }
    }
}

/// 实时调度实体，对应 Linux `struct sched_rt_entity` 的基础字段。
#[derive(Clone, Copy, Debug)]
pub struct SchedRtEntity {
    /// SCHED_RR 当前剩余时间片，单位由调度时钟统一定义。
    pub time_slice: u64,
    /// 任务是否允许加入 SMP 实时迁移队列。
    pub migratable: bool,
}

impl SchedRtEntity {
    /// 创建尚未分配 RR 时间片且允许迁移的实时实体。
    pub const fn new() -> Self {
        Self { time_slice: 0, migratable: true }
    }
}

/// Deadline 调度实体，对应 Linux `struct sched_dl_entity` 的基础 CBS 参数。
#[derive(Clone, Copy, Debug)]
pub struct SchedDlEntity {
    /// 每个周期允许消耗的运行时间。
    pub runtime: u64,
    /// 相对截止时间。
    pub deadline: u64,
    /// 任务周期。
    pub period: u64,
    /// 当前实例剩余运行时间。
    pub remaining_runtime: u64,
    /// 当前实例的绝对截止时间。
    pub absolute_deadline: u64,
    /// CBS runtime 耗尽后是否被限流。
    pub throttled: bool,
}

impl SchedDlEntity {
    /// 创建尚未配置 CBS 参数的 Deadline 实体。
    pub const fn new() -> Self {
        Self {
            runtime: 0,
            deadline: 0,
            period: 0,
            remaining_runtime: 0,
            absolute_deadline: u64::MAX,
            throttled: false,
        }
    }
}