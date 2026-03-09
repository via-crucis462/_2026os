use super::task::*;

use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::fs::File;
use alloc::string::String;

pub struct ProcessControlBlock {
    pub pid: IdHandle,
    pub parent: Option<IdHandle>,
    pub inner: ProcessControlBlockInner,
}

pub struct ProcessControlBlockInner {
    pub pname: String,
    pub tasks: Vec<Arc<TaskControlBlock>>,
}

