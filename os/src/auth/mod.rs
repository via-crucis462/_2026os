//! 权限管理相关
use alloc::task;

use crate::process::current_task;
use bitflags::bitflags;


/// 文件权限信息
pub struct PermStat {
    pub mode: PermModeFlags,     // 权限 + 文件类型
    pub uid: u32,      // 所有者
    pub gid: u32,      // 所有组
}

impl PermStat {
    /// 初始化默认权限，默认全权限，root 用户和 root 组
    pub fn init_all_perm() -> Self {
        Self {
            mode: PermModeFlags::from_bits(0o777).unwrap(), // 默认全权限
            uid: 0,      // root 用户
            gid: 0,      // root 组
        }
    }
    /// 指定权限、用户和组
    pub fn new(mode: PermModeFlags, uid: u32, gid: u32) -> Self {
        Self { mode, uid, gid }
    }
    pub fn set_uid(&mut self, uid: u32) {
        self.uid = uid;
    }
    pub fn set_gid(&mut self, gid: u32) {
        self.gid = gid;
    }
    pub fn set_mode(&mut self, mode: PermModeFlags) {
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
        if uid == 0 {
            true
        } else if self.uid == uid {
            self.mode.contains(PermModeFlags::U_READ)
        } else if self.gid == gid {
            self.mode.contains(PermModeFlags::G_READ)
        } else {
            self.mode.contains(PermModeFlags::O_READ)
        }
    }
    pub fn can_write(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 {
            true
        } else if self.uid == uid {
            self.mode.contains(PermModeFlags::U_WRITE)
        } else if self.gid == gid {
            self.mode.contains(PermModeFlags::G_WRITE)
        } else {
            self.mode.contains(PermModeFlags::O_WRITE)
        }
    }
    pub fn can_execute(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 {
            true
        } else if self.uid == uid {
            self.mode.contains(PermModeFlags::U_EXECUTE)
        } else if self.gid == gid {
            self.mode.contains(PermModeFlags::G_EXECUTE)
        } else {
            self.mode.contains(PermModeFlags::O_EXECUTE)
        }
    }
}

bitflags! {
    pub struct PermModeFlags: u32 {
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

pub struct PermSet {
    pub r: bool,
    pub w: bool,
    pub x: bool,
}