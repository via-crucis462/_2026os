use super::{VfsInode, Stat, Statx, ROOT_DENTRY};
use alloc::sync::Arc;
use alloc::string::String;
use crate::fs::tmpfs::TmpfsDirInode;
use crate::mm::UserBuffer;
use crate::fs::File;
use crate::arch::sbi::console_getchar;
use crate::task::suspend_current_and_run_next;
// 1. /dev 目录本身
pub struct TtyInode;

impl TtyInode {
    pub fn new() -> Self {
        Self
    }
}

// ==========================================
// 身份一：作为 VfsInode，以便能挂载到 /dev/tty
// ==========================================
impl super::VfsInode for TtyInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000, // 核心约束：字符设备标志位
            blksize: 4096,
            ..Default::default()
        }
    }
    
    fn get_statx(&self) -> super::Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn super::VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

// ==========================================
// 身份二：作为 File 接口，处理具体 FD 的 I/O
// ==========================================
impl File for TtyInode {
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    /// 核心约束：安全读取，非阻塞挂起
    fn read(&self, user_buf: UserBuffer) -> usize {
        let mut count = 0;
        // 核心约束：使用 into_iter() 获取指针，严禁越界/原始指针运算
        for byte_ref in user_buf.into_iter() {
            let mut c: usize;
            loop {
                c = console_getchar();
                if c == 13 || c == '\r' as usize {
                    c = 10; // 回车转换行
                }
                
                // 核心约束：获取不到输入则主动释放 CPU，避免死锁 busy-loop
                if c == 0 || c == 0xffffffffffffffff {
                    suspend_current_and_run_next();
                    continue;
                } else {
                    break;
                }
            }
            
            unsafe {
                *byte_ref = c as u8;
            }
            count += 1;
            break; // 每次只读取 1 字节（与你现有的 Stdin 逻辑保持一致，符合控制台标准行缓冲特性）
        }
        count
    }

    /// 核心约束：安全写入，支持多段缓冲区
    fn write(&self, user_buf: UserBuffer) -> usize {
        let mut count = 0;
        // 核心约束：遍历 buffers 进行安全操作
        for buffer in user_buf.buffers {
            // 尝试使用 utf-8 打印。如果是合法字符则批量输出，性能更好
            if let Ok(s) = core::str::from_utf8(buffer) {
                print!("{}", s);
            } else {
                // 如果遇到非标准 UTF-8 二进制数据（部分测试用例会写奇怪的东西），降级为按字节强制打印
                for &b in buffer.iter() {
                    print!("{}", b as char);
                }
            }
            count += buffer.len();
        }
        count
    }

    /// 核心约束：字符设备忽略 offset，直接转发给 read
    fn read_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.read(buf)
    }

    /// 核心约束：字符设备忽略 offset，直接转发给 write
    fn write_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.write(buf)
    }

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o020000, // 字符设备
            blksize: 4096,
            ..Default::default()
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }
}

// 2. /dev/null

pub struct NullInode;

impl VfsInode for NullInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize {
        0 // 读返回 0 (EOF)
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        buf.len() // 写假装全部写成功
    }
    
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 901,
            mode: 0o020666, // 0o020000 表示字符设备 (S_IFCHR)，0o666 表示 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}


// 3. /dev/zero

pub struct ZeroInode;

impl VfsInode for ZeroInode {
    fn read_at(&self, _offset: usize, buf: &mut [u8]) -> usize {
        buf.fill(0); // 缓冲区全填 0
        buf.len()    // 返回填满的长度
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        buf.len() 
    }
    
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 902,
            mode: 0o020666, // 同样是字符设备 rw-rw-rw-
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}
pub struct RtcInode;

impl VfsInode for RtcInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 903,
            mode: 0o020666, // 字符设备
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

// 4. 执行挂载
pub fn mount_devfs() {
    println!("[VFS] Mounting pseudo-filesystem: /dev");
    // 这里用 TmpfsDirInode 替代你之前写的只读的 DevDirInode
    let dev_dentry = ROOT_DENTRY.insert(String::from("dev"), Arc::new(TmpfsDirInode::new()));
    
    dev_dentry.insert(String::from("null"), Arc::new(NullInode));
    dev_dentry.insert(String::from("zero"), Arc::new(ZeroInode));
    dev_dentry.insert(String::from("rtc"), Arc::new(RtcInode));
    
    // shm 共享内存测试必备，里面建的文件直接吃内存，正经的 Tmpfs！
    dev_dentry.insert(String::from("shm"), Arc::new(TmpfsDirInode::new())); 
}

impl NullInode {
    pub fn new() -> Self {
        Self
    }
}

impl ZeroInode {
    pub fn new() -> Self {
        Self
    }
}

// 如果你有 RtcInode，也顺手补一个
impl RtcInode {
    pub fn new() -> Self {
        Self
    }
}