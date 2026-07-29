pub mod runqueue;
pub mod processor;
pub mod wait;
pub mod futex;
pub mod nanosleep;
pub(crate) mod rbtree;
pub mod stop_rq;
pub mod deadline_rq;
pub mod cfs_rq;
pub mod rt_rq;
pub mod idle_rq;

pub use runqueue::*;
pub use processor::*;
pub use wait::*;
pub use futex::*;
pub use nanosleep::*;
pub use stop_rq::*;
pub use deadline_rq::*;
pub use cfs_rq::*;
pub use rt_rq::*;
pub use idle_rq::*;

/// 参与调度的 CPU 数量，与当前架构配置保持一致。
pub const CPU_NUM: usize = crate::arch::config::CPU_CORE_NUM;