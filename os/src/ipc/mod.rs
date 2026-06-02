//！ IPC 进程间通信
//！ pipe等被归于文件系统内容，暂时不放在这里

pub mod msg;
pub mod shm;

/// System V IPC 权限信息
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct IpcPerm {
    pub key: i32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u16,
    pub seq: u16,
}