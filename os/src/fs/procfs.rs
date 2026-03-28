use super::{VfsInode, Stat, Statx, ROOT_DENTRY};
use alloc::sync::Arc;
use alloc::string::String;
use crate::fs::{TmpfsDirInode, TmpfsFileInode};


//造一个“空目录” Inode，专门给 /proc 文件夹用

pub struct ProcDirInode;

impl VfsInode for ProcDirInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 998,
            mode: 0o040555,
            nlink: 2,
            // 下面是补齐的缺漏字段
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 512, // 默认扇区大小
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { 0 }
}


pub struct MemInfoInode;

impl VfsInode for MemInfoInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        if offset > 0 { return 0; }

        let meminfo_str = "MemTotal:        8192 kB\nMemFree:         4096 kB\nMemAvailable:    4096 kB\n";
        
        let bytes = meminfo_str.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        len
    }

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, 
            ino: 999, 
            mode: 0o100444, 
            nlink: 1,
            // 下面是补齐的缺漏字段
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 512,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused:[0; 1],
        }
    }
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}

pub struct MountsInode;

impl VfsInode for MountsInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        if offset > 0 { return 0; }
        // 伪造的标准 Linux 挂载信息表
        let mounts_str = "rootfs / rootfs rw 0 0\nproc /proc proc rw 0 0\ndevtmpfs /dev devtmpfs rw 0 0\n";
        let bytes = mounts_str.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        len
    }

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 997, mode: 0o100444, nlink: 1, // 普通文件只读
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0,
            ctime_sec: 0, ctime_nsec: 0, __unused: [0;1],
        }
    }
    // 把底下那堆 unimplemented 或 None 补齐 (跟 MemInfoInode 一样)
    fn get_statx(&self) -> Statx { unimplemented!() }
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
    fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
    fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { -1 }
}
pub fn mount_procfs() {
    // 1. 创建 /proc 目录
    let proc_dentry = ROOT_DENTRY.insert(String::from("proc"), Arc::new(TmpfsDirInode::new()));
    
    // 2. 塞入特殊的虚拟文件
    proc_dentry.insert(String::from("meminfo"), Arc::new(MemInfoInode));
    proc_dentry.insert(String::from("mounts"), Arc::new(MountsInode));
    
    // 3. 在 /proc 下创建 self 目录
    let self_dentry = proc_dentry.insert(String::from("self"), Arc::new(TmpfsDirInode::new()));
    
    // 4. 在 /proc/self 下创建 maps 空文件
    // 直接用你写好的 TmpfsFileInode，它默认就是一个合法的、可读写的空文件！
    self_dentry.insert(String::from("maps"), Arc::new(TmpfsFileInode::new()));
    
    println!("[VFS] /proc/meminfo, mounts, and /proc/self/maps mounted successfully!");
}