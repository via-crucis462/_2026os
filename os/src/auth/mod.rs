//! 权限管理相关

pub struct PermStat {
    pub mode: u32,     // 权限 + 文件类型
    pub uid: u32,      // 所有者
    pub gid: u32,      // 所有组
}

impl PermStat {
    pub fn init_all_perm() -> Self {
        Self {
            mode: 0o777, // 默认全权限
            uid: 0,      // root 用户
            gid: 0,      // root 组
        }
    }
    pub fn new(mode: u32, uid: u32, gid: u32) -> Self {
        Self { mode, uid, gid }
    }
    pub fn set_uid(&mut self, uid: u32) {
        self.uid = uid;
    }
    pub fn set_gid(&mut self, gid: u32) {
        self.gid = gid;
    }
    pub fn set_mode(&mut self, mode: u32) {
        self.mode = mode;
    }
    pub fn can_read(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.uid == uid {
            self.mode & 0o400 != 0
        } else if self.gid == gid {
            self.mode & 0o040 != 0
        } else {
            self.mode & 0o004 != 0
        }
    }
    pub fn can_write(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.uid == uid {
            self.mode & 0o200 != 0
        } else if self.gid == gid {
            self.mode & 0o020 != 0
        } else {
            self.mode & 0o002 != 0
        }
    }
    pub fn can_execute(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.uid == uid {
            self.mode & 0o100 != 0
        } else if self.gid == gid {
            self.mode & 0o010 != 0
        } else {
            self.mode & 0o001 != 0
        }
    }
}