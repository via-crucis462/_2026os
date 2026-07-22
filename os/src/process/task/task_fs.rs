use crate::fs::Dentry;
use alloc::sync::Arc;
pub struct FsStruct{
    root: Arc<Dentry>, // 根目录
    pwd: Arc<Dentry>, // 当前工作目录
    umask: u32, // 文件创建掩码
}
