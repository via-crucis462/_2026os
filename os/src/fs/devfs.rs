use super::{VfsInode, Stat, Statx, StatxTimestamp, ROOT_DENTRY};
use alloc::sync::Arc;
use alloc::string::String;
use crate::fs::tmpfs::TmpfsDirInode;
use crate::mm::UserBuffer;
use crate::fs::File;
use crate::arch::sbi::console_getchar;
use crate::task::suspend_current_and_run_next;
use crate::fs::ino::get_next_ino;
use spin::Mutex;

/// /dev/tty 字符设备
pub struct TtyInode {
    ino: u64,
}

impl TtyInode {
    pub fn new() -> Self { Self { ino: get_next_ino() } }
}

/// /dev/urandom 随机数设备
pub struct UrandomInode {
    ino: u64,
    seed: Mutex<u32>,
}

impl UrandomInode {
    pub fn new() -> Self {
        Self {
            ino: get_next_ino(),
            seed: Mutex::new(0x12345678),
        }
    }
}

// 实现 /dev/urandom 的 VfsInode trait
impl VfsInode for UrandomInode {
    fn read_at(&self, _offset: usize, buf: &mut [u8]) -> usize {
        let mut seed = self.seed.lock();
        
        for b in buf.iter_mut() {

            *seed = seed.wrapping_mul(1103515245).wrapping_add(12345);

            *b = (*seed >> 16) as u8; 
        }
        buf.len()
    }
    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        // 向 /dev/urandom 写入数据在 Linux 中的语义是“增加系统的熵池”
        // 假装写成功，丢弃数据
        buf.len()
    }
    fn get_size(&self) -> usize { 0 }
    fn ino(&self) -> u64 { self.ino }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: self.ino,
            mode: 0o020666, // S_IFCHR (字符设备 0o020000) | rw-rw-rw- (0666)
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 265,      // 主设备号 1，次设备号 9 (urandom 的标准 rdev)
            __pad: 0,
            size: 0,
            blksize: 512,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0, atime_nsec: 0,
            mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0,
            __unused: [0; 2],
        }
    }
    fn get_statx(&self) -> Statx { 
        let stat = self.get_stat();
        Statx {
            stx_mask: 0,
            stx_blksize: stat.blksize as u32,
            stx_attributes: 0,
            stx_nlink: stat.nlink,
            stx_uid: stat.uid,
            stx_gid: stat.gid,
            stx_mode: stat.mode as u16,
            stx_ino: stat.ino,
            stx_size: stat.size as u64,
            stx_blocks: stat.blocks as u64,
            stx_attributes_mask: 0,
            stx_atime: super::StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
            stx_btime: Default::default(),
            stx_ctime: super::StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
            stx_mtime: super::StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
            stx_rdev_major: 1, // urandom 主设备号
            stx_rdev_minor: 9, // urandom 次设备号
            stx_dev_major: 0,
            stx_dev_minor: 0,
            ..Default::default()
        }
    }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}
// 实现tty为vfs inode
impl super::VfsInode for TtyInode {
    // 读取终端输入
    fn read_at(&self, _offset: usize, buf: &mut [u8]) -> usize {
        if buf.is_empty() { return 0; }
        let mut c: usize;
        loop {
            c = console_getchar();
            if c == 13 || c == '\r' as usize {
                c = 10; // 回车转换行
            }
            if c == 0 || c == 0xffffffffffffffff {
                suspend_current_and_run_next(); // 非阻塞挂起
                continue;
            } else {
                break;
            }
        }
        buf[0] = c as u8;
        1
    }
    // 终端输出
    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize { 
        let str = core::str::from_utf8(buf).unwrap_or("<invalid utf-8>");
        print!("{}", str);
        buf.len()
    }
    fn get_size(&self) -> usize { 0 }
    fn ino(&self) -> u64 { self.ino }
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            ino: self.ino,
            mode: 0o020000, // 字符设备标志位 (S_IFCHR)
            blksize: 4096,
            ..Default::default()
        }
    }
    // 保持默认/空实现
    fn get_statx(&self) -> super::Statx { stat_to_statx(&self.get_stat()) }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

pub struct NullInode {
    ino: u64,
}

impl VfsInode for NullInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize {
        0 // 读返回 0 (EOF)
    }
    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        // 忽略写操作
        buf.len()
    }
    fn get_size(&self) -> usize { 0 }
    fn ino(&self) -> u64 { self.ino }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: self.ino,
            mode: 0o020666, // 0o020000 表示字符设备 (S_IFCHR)，0o666 表示 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    fn get_statx(&self) -> Statx { stat_to_statx(&self.get_stat()) }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}


// /dev/zero
pub struct ZeroInode {
    ino: u64,
}

impl ZeroInode {
    pub fn new() -> Self {
         Self {
            ino: get_next_ino()
        }
    }
}

impl VfsInode for ZeroInode {
    fn read_at(&self, _offset: usize, buf: &mut [u8]) -> usize {
        buf.fill(0); // 缓冲区全填 0
        buf.len()
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        buf.len() 
    }
    
    fn get_size(&self) -> usize { 0 }
    fn ino(&self) -> u64 { self.ino }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: self.ino,
            mode: 0o020666, // 同样是字符设备 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    fn get_statx(&self) -> Statx { stat_to_statx(&self.get_stat()) }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}
pub struct RtcInode {
    ino: u64,
}

impl RtcInode {
    pub fn new() -> Self {
        Self {
            ino: get_next_ino()
        }
    }
}

impl VfsInode for RtcInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn ino(&self) -> u64 { self.ino }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: self.ino,
            mode: 0o020666, // 字符设备
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0; 2],
        }
    }
    fn get_statx(&self) -> Statx { stat_to_statx(&self.get_stat()) }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

/* 挂载 /dev 设备文件系统
pub fn mount_devfs() {
    info!("[VFS] Mounting pseudo-filesystem: /dev");
    // 这里用 TmpfsDirInode 替代你之前写的只读的 DevDirInode
    let dev_dentry = ROOT_DENTRY.insert(String::from("dev"), Arc::new(TmpfsDirInode::new(0o777)));
    
    dev_dentry.insert(String::from("null"), Arc::new(NullInode));
    dev_dentry.insert(String::from("zero"), Arc::new(ZeroInode));
    dev_dentry.insert(String::from("rtc"), Arc::new(RtcInode));
    
    // shm 共享内存目录，内部是共享内存文件
    dev_dentry.insert(String::from("shm"), Arc::new(TmpfsDirInode::new(0o777))); 
} */

impl NullInode {
    pub fn new() -> Self {
        Self { ino: get_next_ino() }
    }
}

// Convert a `Stat` to `Statx` for VFS implementations.
fn stat_to_statx(stat: &Stat) -> Statx {
    Statx {
        stx_mask: 0,
        stx_blksize: stat.blksize as u32,
        stx_attributes: 0,
        stx_nlink: stat.nlink,
        stx_uid: stat.uid,
        stx_gid: stat.gid,
        stx_mode: stat.mode as u16,
        __spare0: [0u16; 1],
        stx_ino: stat.ino,
        stx_size: stat.size as u64,
        stx_blocks: stat.blocks as u64,
        stx_attributes_mask: 0,
        stx_atime: StatxTimestamp { tv_sec: stat.atime_sec, tv_nsec: stat.atime_nsec as u32, __reserved: 0 },
        stx_btime: StatxTimestamp { tv_sec: 0, tv_nsec: 0, __reserved: 0 },
        stx_ctime: StatxTimestamp { tv_sec: stat.ctime_sec, tv_nsec: stat.ctime_nsec as u32, __reserved: 0 },
        stx_mtime: StatxTimestamp { tv_sec: stat.mtime_sec, tv_nsec: stat.mtime_nsec as u32, __reserved: 0 },
        stx_rdev_major: (stat.rdev >> 32) as u32,
        stx_rdev_minor: stat.rdev as u32,
        stx_dev_major: (stat.dev >> 32) as u32,
        stx_dev_minor: stat.dev as u32,
        __spare2: [0u64; 14],
    }
}