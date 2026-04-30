use super::{VfsInode, Stat, Statx, ROOT_DENTRY};
use alloc::sync::Arc;
use alloc::string::{String, ToString};
use crate::fs::{TmpfsDirInode, TmpfsFileInode, stat_to_statx};
use core::fmt::{self, Write};
use crate::mm::get_free_frames;
use crate::task::get_process;
use crate::syscall::fs::Statfs;
use core::sync::atomic::Ordering;
use alloc::format;
macro_rules! impl_default_statx {
    () => {
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
                stx_atime: super::StatxTimestamp {
                    tv_sec: stat.atime_sec,
                    tv_nsec: stat.atime_nsec as u32,
                    __reserved: 0,
                },
                stx_btime: Default::default(),
                stx_ctime: super::StatxTimestamp {
                    tv_sec: stat.ctime_sec,
                    tv_nsec: stat.ctime_nsec as u32,
                    __reserved: 0,
                },
                stx_mtime: super::StatxTimestamp {
                    tv_sec: stat.mtime_sec,
                    tv_nsec: stat.mtime_nsec as u32,
                    __reserved: 0,
                },
                stx_rdev_major: 0, 
                stx_rdev_minor: 0, 
                stx_dev_major: 0, 
                stx_dev_minor: 0, 
                ..Default::default()
            }
        }
    };
}

/// 统一实现不支持的目录操作（带 getdents 返回值参数，文件传 -1，目录传 0）
macro_rules! impl_unsupported_ops {
    ($getdents_ret:expr) => {
        fn create_file(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
        fn create_dir(&self, _name: &str, _mode: u32) -> Option<Arc<dyn VfsInode>> { None }
        fn delete_dir_entry(&self, _name: &str) -> Option<u32> { None }
        fn getdents(&self, _offset: &mut usize, _buf: &mut [u8]) -> isize { $getdents_ret }
    };
}
pub struct ProcPidDirInode {
    pub pid: usize,
}

impl VfsInode for ProcPidDirInode {
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        match name {
            // 当查找 oom_score_adj 时，返回一个绑定了该 PID 的特殊文件
            "oom_score_adj" => Some(Arc::new(OomScoreAdjInode { pid: self.pid })),
            

            "status" => Some(Arc::new(ProcStatusInode { pid: self.pid })),
            "ns" => Some(Arc::new(ProcNsDirInode { pid: self.pid })),
            // "maps" => Some(Arc::new(ProcMapsInode { pid: self.pid })),
            
            _ => None,
        }
    }

    // --- 目录占位方法 ---
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0, 
            ino: (10000 + self.pid) as u64, // 用 pid 生成一个假 ino 防止冲突
            mode: 0o040555, // 动态目录给只读和执行权限
            nlink: 2,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
   
    fn statfs(&self) -> Statfs {
        Statfs {
            f_type: 0x01021994, f_bsize: 4096, f_blocks: 0, 
            f_bfree: 0, f_bavail: 0, f_files: 0, f_ffree: 0,
            f_fsid: [0, 0], f_namelen: 255, f_frsize: 4096, f_flags: 0, f_spare: [0; 4],
        }
    }
    impl_default_statx!();
    impl_unsupported_ops!(0);
}
pub struct OomScoreAdjInode {
    pub pid: usize,
}

impl VfsInode for OomScoreAdjInode {
    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { 
        None 
    }
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        // 1. 去进程管理器里获取真实的 PCB
        let score = if let Some(process) = get_process(self.pid) {
            // 直接无锁读取里面真实的 oom_score_adj 值！
            process.oom_score_adj.load(Ordering::SeqCst)
        } else {
            0 // 如果进程刚巧退出了，默认返回 0
        };

        // 2. 格式化为字符串
        let score_str = format!("{}\n", score);
        let score_bytes = score_str.as_bytes();
        
        // 3. 处理偏移和复制给用户态
        if offset >= score_bytes.len() {
            return 0;
        }
        let read_len = core::cmp::min(buf.len(), score_bytes.len() - offset);
        buf[..read_len].copy_from_slice(&score_bytes[offset..offset + read_len]);
        read_len
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> usize {
        // 1. 解析 LTP 传进来的 "-1000" 等字符串
        let s = core::str::from_utf8(buf).unwrap_or("").trim();
        if let Ok(score) = s.parse::<i32>() {
            // 2. 找到对应进程的 PCB，并将值无锁地保存进去！
            if let Some(process) = get_process(self.pid) {
                process.oom_score_adj.store(score, Ordering::SeqCst);
            }
        }
        // 返回写入长度，告知系统调用成功
        buf.len()
    }
    fn get_size(&self) -> usize { 0 }
fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0, 
            ino: 998, 
           
            mode: 0o100666, 
            nlink: 1, 
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
   impl_default_statx!();
    impl_unsupported_ops!(0);
    
    fn statfs(&self) -> Statfs {
        Statfs {
            f_type: 0x01021994, f_bsize: 4096, f_blocks: 0, 
            f_bfree: 0, f_bavail: 0, f_files: 0, f_ffree: 0,
            f_fsid: [0, 0], f_namelen: 255, f_frsize: 4096, f_flags: 0, f_spare: [0; 4],
        }
    }

}
pub struct ProcDirInode;

struct StackBuffer<'a> {
    buf: &'a mut [u8],
    len: usize,
}
pub struct ProcRootInode {
    static_entries: TmpfsDirInode, 
}

impl ProcRootInode {
    pub fn new() -> Self {
        Self { static_entries: TmpfsDirInode::new() }
    }
    pub fn insert_static(&self, name: String, inode: Arc<dyn VfsInode>) {
        self.static_entries.insert(name, inode);
    }
}

impl VfsInode for ProcRootInode {
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        if let Some(inode) = self.static_entries.find(name) {
            return Some(inode);
        }

        if let Ok(pid) = name.parse::<usize>() {

            if get_process(pid).is_some() { 
                return Some(Arc::new(ProcPidDirInode { pid }));
            }
        }

        None
    }

    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0, 
            ino: 998, // 给一个固定的 inode 号
            mode: 0o040555, 
            nlink: 2,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
    impl_default_statx!();
    impl_unsupported_ops!(0);
    
    fn statfs(&self) -> Statfs {
        Statfs {
            f_type: 0x01021994, f_bsize: 4096, f_blocks: 0, 
            f_bfree: 0, f_bavail: 0, f_files: 0, f_ffree: 0,
            f_fsid: [0, 0], f_namelen: 255, f_frsize: 4096, f_flags: 0, f_spare: [0; 4],
        }
    }
}
impl<'a> Write for StackBuffer<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remain = self.buf.len() - self.len;
        if remain < bytes.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}
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
   
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    impl_default_statx!();
    impl_unsupported_ops!(0);
}

pub struct ProcStatusInode {
    pub pid: usize,
}

impl VfsInode for ProcStatusInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {

        let process = match get_process(self.pid) {
            Some(p) => p,
            None => return 0, 
        };

        let (uid, euid, gid, egid) = {
 
            let inner = process.inner.exclusive_access(); 
            (inner.uid, inner.euid, inner.gid, inner.egid)
        };


        let status_str = format!(
            "Name:\toscomp_proc\nState:\tR (running)\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nGroups:\t0\n",
            uid, euid, uid, uid, 
            gid, egid, gid, gid
        );

        let status_bytes = status_str.as_bytes();
        

        if offset >= status_bytes.len() {
            return 0;
        }
        let read_len = core::cmp::min(buf.len(), status_bytes.len() - offset);
        buf[..read_len].copy_from_slice(&status_bytes[offset..offset + read_len]);
        read_len
    }

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize {

        0
    }

    fn find(&self, _name: &str) -> Option<Arc<dyn super::VfsInode>> { None }
    fn get_size(&self) -> usize { 0 } 

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            dev: 0, 
            ino: 999, 
            mode: 0o100444, 
            nlink: 1,
            uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    
   impl_default_statx!();
    impl_unsupported_ops!(0);
}
pub struct ProcSelfSymlinkInode;

impl VfsInode for ProcSelfSymlinkInode {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
 
        let current_task = crate::task::current_task().unwrap();
        let pid = current_task.getpid();
        

        let target = pid.to_string();
        let data = target.as_bytes();


        if offset >= data.len() {
            return 0;
        }
        let read_len = core::cmp::min(buf.len(), data.len() - offset);
        buf[..read_len].copy_from_slice(&data[offset..offset + read_len]);
        read_len
    }

    fn get_stat(&self) -> Stat {

        let pid = crate::task::current_task().unwrap().getpid();
        let target_len = pid.to_string().len();

        Stat {
            dev: 0,
            ino: 1,        
            mode: 0o120777, 
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: target_len as i64, 
            blksize: 512,
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

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    
    
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
    impl_default_statx!();
    impl_unsupported_ops!(-1);
}
pub struct ProcNsDirInode {
    pub pid: usize,
}
impl VfsInode for ProcNsDirInode {
    fn find(&self, name: &str) -> Option<Arc<dyn VfsInode>> {
        match name {
            "pid" | "net" | "mnt" | "uts" | "ipc" | "user" | "cgroup" => {
                Some(Arc::new(ProcNsFileInode {
                    _pid: self.pid,
                    ns_type: String::from(name),
                }))
            }
            _ => None,
        }
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0, ino: 2, 
            mode: 0o040555, // S_IFDIR (0o040000) | r-xr-xr-x (0o555) 目录权限
            nlink: 2, uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }

    
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    impl_default_statx!();
    impl_unsupported_ops!(0);
}
pub struct ProcNsFileInode {
    pub _pid: usize,
    pub ns_type: String,
}

impl VfsInode for ProcNsFileInode {
    fn read_at(&self, _offset: usize, _buf: &mut [u8]) -> usize { 0 }
    fn write_at(&self, _offset: usize, _buf: &[u8]) -> usize { 0 }

    fn get_stat(&self) -> Stat {
        // 给不同的 ns 类型分配真实的 Linux 默认 Inode 编号
        let ino = match self.ns_type.as_str() {
            "pid" => 4026531836,
            "mnt" => 4026531840,
            "net" => 4026531992,
            "uts" => 4026531838,
            "ipc" => 4026531839,
            "user" => 4026531837,
            "cgroup" => 4026531835,
            _ => 9999,
        };

        Stat {
            dev: 0, 
            ino, 
            mode: 0o100444, 
            nlink: 1, uid: 0, gid: 0, rdev: 0, __pad: 0, size: 0, blksize: 512, __pad2: 0,
            blocks: 0, atime_sec: 0, atime_nsec: 0, mtime_sec: 0, mtime_nsec: 0, ctime_sec: 0, ctime_nsec: 0, __unused: [0; 1],
        }
    }
    fn get_size(&self) -> usize { 0 }
    impl_default_statx!();
    impl_unsupported_ops!(0);
    fn find(&self, _name: &str) -> Option<Arc<dyn VfsInode>> { None }
}
pub struct MemInfoInode;

impl VfsInode for MemInfoInode {
   fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let free_frames = get_free_frames(); 
        let free_kb = free_frames * 4;
        let total_kb = 128 * 1024; 
        let mut local_buf = [0u8; 128];
        let mut writer = StackBuffer { buf: &mut local_buf, len: 0 };
        let _ = write!(
            writer,
            "MemTotal:        {} kB\nMemFree:         {} kB\nMemAvailable:    {} kB\n",
            total_kb, free_kb, free_kb
        );

        let output_bytes = &writer.buf[..writer.len];

        if offset >= output_bytes.len() {
            return 0; // 读到文件末尾
        }

        let read_len = core::cmp::min(buf.len(), output_bytes.len() - offset);
        buf[..read_len].copy_from_slice(&output_bytes[offset..offset + read_len]);
        
        read_len
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
    fn get_statx(&self) -> Statx { 
        let stat = self.get_stat();
        Statx{
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
            stx_atime: super::StatxTimestamp {
                tv_sec: stat.atime_sec,
                tv_nsec: stat.atime_nsec as u32,
                __reserved: 0,
            },
            stx_btime: Default::default(),
            stx_ctime: super::StatxTimestamp {
                tv_sec: stat.ctime_sec,
                tv_nsec: stat.ctime_nsec as u32,
                __reserved: 0,
            },
            stx_mtime: super::StatxTimestamp {
                tv_sec: stat.mtime_sec,
                tv_nsec: stat.mtime_nsec as u32,
                __reserved: 0,
            },
            stx_rdev_major: 0,
            stx_rdev_minor: 0,
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
    fn get_statx(&self) -> Statx { 
        let stat = self.get_stat();
        Statx{
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
            stx_atime: super::StatxTimestamp {
                tv_sec: stat.atime_sec,
                tv_nsec: stat.atime_nsec as u32,
                __reserved: 0,
            },
            stx_btime: Default::default(),
            stx_ctime: super::StatxTimestamp {
                tv_sec: stat.ctime_sec,
                tv_nsec: stat.ctime_nsec as u32,
                __reserved: 0,
            },
            stx_mtime: super::StatxTimestamp {
                tv_sec: stat.mtime_sec,
                tv_nsec: stat.mtime_nsec as u32,
                __reserved: 0,
            },
            stx_rdev_major: 0,
            stx_rdev_minor: 0,
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

pub fn mount_procfs() {
    let proc_root = Arc::new(ProcRootInode::new());
    let sys_dir = Arc::new(TmpfsDirInode::new());
    let kernel_dir = Arc::new(TmpfsDirInode::new());
    kernel_dir.insert(
        String::from("tainted"), 
        Arc::new(TmpfsFileInode::new_with_data(b"0\n"))
    );
    kernel_dir.insert(
        String::from("pid_max"), 
        Arc::new(TmpfsFileInode::new_with_data(b"32768\n"))
    );
    sys_dir.insert(String::from("kernel"), kernel_dir);
    proc_root.insert_static(String::from("sys"), sys_dir);
    proc_root.insert_static(String::from("meminfo"), Arc::new(MemInfoInode));
    proc_root.insert_static(String::from("mounts"), Arc::new(MountsInode));
    let self_dentry = Arc::new(TmpfsDirInode::new());
    self_dentry.insert(
        String::from("oom_score_adj"), 
        Arc::new(TmpfsFileInode::new_with_data(b"0\n"))
    );
    self_dentry.insert(String::from("maps"), Arc::new(TmpfsFileInode::new()));
    
    proc_root.insert_static(String::from("self"), Arc::new(ProcSelfSymlinkInode));
    // 4. 正式把完整的动态 /proc 挂载到操作系统的 ROOT_DENTRY！
    ROOT_DENTRY.insert(String::from("proc"), proc_root);
   
    
    info!("[VFS] /proc/meminfo, mounts, and /proc/self/maps mounted successfully!");
}