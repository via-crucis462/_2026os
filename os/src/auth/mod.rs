//! 权限管理相关
use alloc::task;

use crate::process::current_task;
use bitflags::bitflags;


/// 文件权限信息，Stat的子集
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermStat {
    pub mode: FileMode,
    pub uid: u32,      // 文件owner用户
    pub gid: u32,      // 文件owner组
}

impl PermStat {
    /// 指定权限、用户和组
    pub fn new(mode: FileMode, uid: u32, gid: u32) -> Self {
        Self { mode, uid, gid }
    }
    pub fn set_uid(&mut self, uid: u32) {
        self.uid = uid;
    }
    pub fn set_gid(&mut self, gid: u32) {
        self.gid = gid;
    }
    pub fn set_mode(&mut self, mode: FileMode) {
        self.mode = mode;
    }
    /// 获取当前用户对目标文件的权限集合
    pub fn current_perm_set(&self) -> PermSet {
        let proc = current_task().unwrap().process.upgrade().unwrap();
        let inner = proc.inner_exclusive_access();
        let uid = inner.uid;
        let gid = inner.gid;
        drop(inner);
        drop(proc);
        PermSet {
            w: self.can_write(uid, gid),
            r: self.can_read(uid, gid),
            x: self.can_execute(uid, gid),
        }
    }
    pub fn can_read(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 { // root用户有全部权限，跳过鉴权
            true
        } else if self.uid == uid {
            self.mode.contains(FileMode::U_READ)
        } else if self.gid == gid {
            self.mode.contains(FileMode::G_READ)
        } else {
            self.mode.contains(FileMode::O_READ)
        }
    }
    pub fn can_write(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 { // root用户有全部权限，跳过鉴权
            true
        } else if self.uid == uid {
            self.mode.contains(FileMode::U_WRITE)
        } else if self.gid == gid {
            self.mode.contains(FileMode::G_WRITE)
        } else {
            self.mode.contains(FileMode::O_WRITE)
        }
    }
    pub fn can_execute(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 { // root用户有全部权限，跳过鉴权
            true
        } else if self.uid == uid {
            self.mode.contains(FileMode::U_EXECUTE)
        } else if self.gid == gid {
            self.mode.contains(FileMode::G_EXECUTE)
        } else {
            self.mode.contains(FileMode::O_EXECUTE)
        }
    }
}

bitflags! {
    pub struct FileMode: u16 {
        // 文件类型掩码
        const S_IFMT   = 0o170000;
        // 文件类型
        const S_IFSOCK = 0o140000; // 套接字
        const S_IFLNK  = 0o120000; // 符号链接
        const S_IFREG  = 0o100000; // 普通文件
        const S_IFBLK  = 0o060000; // 块设备
        const S_IFDIR  = 0o040000; // 目录
        const S_IFCHR  = 0o020000; // 字符设备
        const S_IFIFO  = 0o010000; // FIFO/管道
        // --- 以上鉴权时忽略，这里留作参考和后续拓展用 ---

        // 特殊权限位
        const S_ISUID  = 0o004000;
        const S_ISGID  = 0o002000;
        const S_ISVTX  = 0o001000;

        // 用户权限位
        const U_READ = 0o400;
        const U_WRITE = 0o200;
        const U_EXECUTE = 0o100;
        const G_READ = 0o040;
        const G_WRITE = 0o020;
        const G_EXECUTE = 0o010;
        const O_READ = 0o004;
        const O_WRITE = 0o002;
        const O_EXECUTE = 0o001;
    }
}

impl FileMode {
    /// 提取文件类型
    pub fn file_type(self) -> Self {
        self & Self::S_IFMT
    }
    /// 是否是普通文件
    pub fn is_regular_file(self) -> bool {
        self.file_type() == Self::S_IFREG
    }
    /// 是否是目录
    pub fn is_dir(self) -> bool {
        self.file_type() == Self::S_IFDIR
    }
    /// 是否是符号链接
    pub fn is_symlink(self) -> bool {
        self.file_type() == Self::S_IFLNK
    }
}

pub struct PermSet {
    pub r: bool,
    pub w: bool,
    pub x: bool,
}