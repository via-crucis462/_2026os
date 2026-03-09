use crate::sync::MPSafeCell;
use alloc::vec::Vec;
use super::{TaskControlBlock};

pub struct TaskPool {
    inner: MPSafeCell<Vec<TaskControlBlock>>,
}

pub struct Scheduler {
    task_pool: TaskPool,
}