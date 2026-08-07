//! File and filesystem-related syscalls
use crate::PAGE_SIZE;
use crate::auth::FileMode;
use crate::process::FdFlags;
use crate::fs::{create_fifo_in_dentry, create_file_in_dentry, is_fifo_mode, make_pipe, open_fifo_file, Dentry, File, OpenFlags, ROOT_DENTRY, Stat, Statx, file_name, make_dir, open_file, parent_path, S_IFMT, UserPageFaultInfo};
use crate::mm::{PageSize, UserBuffer, prepare_user_read, prepare_user_write, translated_byte_buffer, translated_read, try_translated_read, try_translated_str, try_translated_write};
use crate::task::{current_task, current_user_token};
use alloc::{task, vec};
use alloc::sync::Arc;
use alloc::string::ToString;
use crate::syscall::TIME_CACHE;
use super::{errno::Errno::*, normalize_leading_dot_path, translate_path};
use crate::syscall::TmpfsFileInode;
use crate::syscall::OSInode;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use crate::process::scheduler::wait::{block_current_and_run_next_if_mp, wake_up_all_mp};
use crate::sync::MPSafeCell;
use crate::sync::WaitQueue;
use lazy_static::lazy_static;

const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_GETLK: usize = 5;
const F_SETLK: usize = 6;
const F_SETLKW: usize = 7;
const F_DUPFD_CLOEXEC: usize = 1030;
const F_GETPIPE_SIZE: usize = 1032;
const FD_CLOEXEC: usize = 1;
const O_ACCMODE: usize = 0o3;
const O_NONBLOCK: usize = 0o4000;
const O_NDELAY: usize = O_NONBLOCK;
const O_CLOEXEC: u32 = 0o2000000;

const O_RDONLY: u32 = 0;
const O_WRONLY: u32 = 0o1;
const O_RDWR: u32 = 0o2;


use super::errno::Errno::*;

const AT_REMOVEDIR: usize = 0x200;

const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;
const SEEK_SET: i16 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Flock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
    _pad: i32,
}

#[derive(Clone, Copy)]
struct RecordLock {
    owner_pid: usize,
    lock_type: i16,
    start: u64,
    len: Option<u64>,
}

lazy_static! {
    static ref RECORD_LOCKS: MPSafeCell<BTreeMap<u64, Vec<RecordLock>>> =
        MPSafeCell::new(BTreeMap::new());
    static ref RECORD_LOCK_WAITERS: MPSafeCell<WaitQueue> = MPSafeCell::new(WaitQueue::new());
}

fn flock_range(flock: &Flock) -> Option<(u64, Option<u64>)> {
    if flock.l_whence != SEEK_SET || flock.l_start < 0 || flock.l_len < 0 {
        return None;
    }
    let start = flock.l_start as u64;
    let len = (flock.l_len != 0).then_some(flock.l_len as u64);
    Some((start, len))
}

fn ranges_overlap(left_start: u64, left_len: Option<u64>, right_start: u64, right_len: Option<u64>) -> bool {
    let left_end = left_len.and_then(|len| left_start.checked_add(len));
    let right_end = right_len.and_then(|len| right_start.checked_add(len));
    match (left_end, right_end) {
        (Some(left_end), Some(right_end)) => left_start < right_end && right_start < left_end,
        (Some(left_end), None) => right_start < left_end,
        (None, Some(right_end)) => left_start < right_end,
        (None, None) => true,
    }
}

fn release_record_locks(pid: usize, inode: u64) {
    let mut locks = RECORD_LOCKS.exclusive_access();
        let previous_len;
        let released;
        if let Some(entries) = locks.get_mut(&inode) {
            previous_len = entries.len();
            entries.retain(|entry| entry.owner_pid != pid);
            released = entries.len() != previous_len;
            if entries.is_empty() {
                locks.remove(&inode);
            }
        } else {
            previous_len = 0;
            released = false;
        }
        drop(locks);
        if released {
            wake_up_all_mp(&RECORD_LOCK_WAITERS);
        }
}

fn has_record_lock_conflict(inode: u64, pid: usize, lock_type: i16, start: u64, len: Option<u64>) -> bool {
    RECORD_LOCKS
        .exclusive_access()
        .get(&inode)
        .is_some_and(|entries| entries.iter().any(|entry| {
            entry.owner_pid != pid
                && (entry.lock_type == F_WRLCK || lock_type == F_WRLCK)
                && ranges_overlap(entry.start, entry.len, start, len)
        }))
}

fn current_files() -> Arc<crate::sync::MPSafeCell<crate::process::FileDescriptorTable>> {
    current_task().unwrap().inner_exclusive_access().files.clone()
}

fn current_nofile_limit() -> usize {
    current_task().unwrap().nofile_limit()
}

fn current_pwd() -> Arc<Dentry> {
    let fs = current_task().unwrap().inner_exclusive_access().fs.clone();
    let pwd = fs.exclusive_access().get_pwd();
    pwd
}

fn current_umask() -> u32 {
    let fs = current_task().unwrap().inner_exclusive_access().fs.clone();
    let mask = fs.exclusive_access().umask();
    mask
}

fn current_euid() -> u32 {
    let cred = current_task().unwrap().inner_exclusive_access().cred.clone();
    let euid = cred.exclusive_access().euid();
    euid
}
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Statfs {
    pub f_type: u64,    // 文件系统类型
    pub f_bsize: u64,   // 最佳传输块大小 (通常 4096)
    pub f_blocks: u64,  // 磁盘总块数
    pub f_bfree: u64,   // 剩余块数
    pub f_bavail: u64,  // 普通用户可用的剩余块数
    pub f_files: u64,   // 总 inode 节点数
    pub f_ffree: u64,   // 剩余 inode 节点数
    pub f_fsid: [u32; 2], // 文件系统 ID
    pub f_namelen: u64, // 最大文件名长度
    pub f_frsize: u64,  // 碎片大小
    pub f_flags: u64,   // 挂载标志
    pub f_spare: [u64; 4], // 保留字段
}
pub fn sys_statfs(path: *const u8, buf: *mut Statfs) -> isize {
    let token = current_user_token();
    let path_str = {
        if let Some(s) = try_translated_str(token, path) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    trace!("kernel:pid[{}] sys_statfs path={}", current_task().unwrap().getpid(), path_str);

    if buf.is_null() {
        return EFAULT.as_isize();
    }

    // 1. 根据传入的路径，从目录树中找到对应的文件/目录节点
    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else {
        current_pwd()
    };
    let target_dentry = if path_str == "/" {
        crate::fs::ROOT_DENTRY.clone()
    } else if let Ok(dentry) = start_dentry.find_tree(&path_str, true) {
        dentry
    } else {
        return ENOENT.as_isize(); // 路径不存在，拒绝伪造！
    };

    // 2. 多态调用 
    let stat = target_dentry.inode.statfs();

    // 3. 将真实数据写入用户空间
    if !try_translated_write(token, buf, stat) {
        return EFAULT.as_isize();
    }
    
    0 // Success!
}

/// 返回打开文件的stat
pub fn sys_fstatfs(fd: usize, buf: *mut Statfs) -> isize {
    if buf.is_null() {
        return EFAULT.as_isize();
    }

    let file = {
        let files = current_files();
        let inner = files.exclusive_access();
        if fd >= inner.fds.len() {
            return EBADF.as_isize();
        }
        let Some(file) = inner.fds[fd].file.as_ref() else {
            return EBADF.as_isize();
        };
        file.clone()
    };

    let Some(dentry) = file.get_dentry() else {
        return ENOSYS.as_isize();
    };
    let stat = dentry.inode.statfs();
    if !try_translated_write(current_user_token(), buf, stat) {
        return EFAULT.as_isize();
    }
    0
}

fn ensure_fd_slots(files: &mut crate::process::FileDescriptorTable, target_len: usize) -> bool {
    files.ensure_slots(target_len, current_nofile_limit())
}
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    let token = current_user_token();
    // write(2) copies bytes from user space into the kernel/file.  The user
    // buffer only needs to be readable; requiring write permission rejects
    // valid string literals and other read-only mappings with EFAULT.
    if !prepare_user_read(token, buf as usize, len) {
        return EFAULT.as_isize();
    }
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    info!("pid[{}] [sys_write] ENTER fd={}, buf={:#x}, len={}", task.getpid(), fd, buf as usize, len);
    // 检查 FD 是否越界或未打开
    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize(); // 注意引入正确的 EBADF 路径
    }
    let file = inner.fds[fd].file.as_ref().unwrap().clone();
    //file.info_type();
    let status = inner.fds[fd].status;
        drop(inner);
        let is_sock = file.is_socket();
        let nonblock = (status & (O_NONBLOCK | O_NDELAY)) != 0;
    if !is_sock && !file.writable() {
        warn!("pid[{}] [sys_write] EACCES fd={} readable={} writable={}",
              task.getpid(), fd, file.readable(), file.writable());
        return EACCES.as_isize(); 
    }
    if is_sock && !file.writable() {
        if nonblock {
            return EAGAIN.as_isize(); 
        }
    }
    if let Some(err) = file.check_write_error() {
        return err.as_isize();
    }
    if (status & (O_NONBLOCK | O_NDELAY)) != 0 && !file.ready_to_write() {
        return EAGAIN.as_isize();
    }
    let nonblock = (status & (O_NONBLOCK | O_NDELAY)) != 0;
    let user_buffer = UserBuffer::new(crate::mm::translated_byte_buffer(token, buf, len));

    let ax = if nonblock {
        match file.write_nonblock(user_buffer) {
            Ok(written) => written as isize,
            Err(err) => return err.as_isize(),
        }
    } else {
        file.write(user_buffer) as isize
    };
    if ax == 0 && len > 0 {
        if let Some(err) = file.check_write_error() {
            return err.as_isize();
        }
    }
    if ax == 0 && len > 0 {
        warn!("pid[{}] [sys_write] FATAL: Underlying file returned 0 on write! fd={}", task.getpid(), fd);
    } else {
        info!("pid[{}] [sys_write] LEAVE written={}", task.getpid(), ax);
    }
  
    ax
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    warn!("kernel:pid[{}] sys_read, aim fd = {}, buf = {:#x}, len = {}", current_task().unwrap().getpid(), fd, buf as usize, len);
    let token = current_user_token();
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fds[fd].file {
        let file = file.clone();
        let status = inner.fds[fd].status;
        drop(inner);
        let is_sock = file.is_socket();
        if !is_sock && !file.readable() {
            println!("EACCES");
            return EACCES.as_isize(); 
        }
        if is_sock && !file.readable() {
            if (status & (O_NONBLOCK | O_NDELAY)) != 0 {
                return EAGAIN.as_isize(); 
            }
        }
        if (status & (O_NONBLOCK | O_NDELAY)) != 0 && !file.ready_to_read() {
            return EAGAIN.as_isize();
        }
        if let Some(pipe) = file.as_any().downcast_ref::<crate::fs::Pipe>() {
            return match pipe.read_for_syscall(UserBuffer::new(translated_byte_buffer(token, buf, len))) {
                Ok(read) => read as isize,
                Err(err) => err.as_isize(),
            };
        }
        //file.info_type();
        let read = file.read(UserBuffer::new(translated_byte_buffer(token, buf, len)));
        // 如果 read 返回 0 且 len > 0，检查是否有读错误
        // 当前实现有个问题，在读异常时重新检查，可能有竞态问题，
        // 后续应该改成让读直接带错误返回。
        if read == 0 && len > 0 {
            if let Some(err) = file.check_read_error() {
                return err.as_isize();
            }
        }
        read as isize
    } else {
        EBADF.as_isize() // 文件描述符无效
    }
}
pub fn sys_readv(fd: usize, iov_ptr: usize, iovcnt: usize) -> isize {
    // 防止随机/恶意 iovcnt 导致死循环或 DOS
    const IOV_MAX: usize = 1024;
    let iovcnt = iovcnt.min(IOV_MAX);
    let files = current_files();
    let inner = files.exclusive_access();
    // fd合法性检查
    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let file = inner.fds[fd].file.as_ref().unwrap().clone();
    let token = current_user_token();
    drop(inner);

    let mut total_read = 0;
    //遍历数组
    for i in 0..iovcnt {
        let iov_addr = iov_ptr + i * core::mem::size_of::<IoVec>();
        // 从虚拟地址解引
        let iovec: IoVec = {
            if let Some(io) = try_translated_read(token, iov_addr as *const IoVec) {
                io
            } else {
                return EFAULT.as_isize();
            }
        };
        if iovec.len == 0 {
            continue;
        }
        // 防止随机/恶意 iovec.len 导致堆分配溢出
        const IOV_BUF_MAX: usize = 1024 * 1024; // 1MB
        let iovec_len = iovec.len.min(IOV_BUF_MAX);
        // 写入缓冲区
        let user_buffer = crate::mm::UserBuffer {
            buffers: crate::mm::translated_byte_buffer_mut(token, iovec.base as *const u8, iovec_len),
        };
        let read_bytes = file.read(user_buffer);
        if read_bytes == 0 && iovec_len > 0 {
            if let Some(err) = file.check_read_error() {
                return if total_read == 0 {
                    err.as_isize()
                } else {
                    total_read as isize
                };
            }
        }
        total_read += read_bytes;
        // 读到底了
        if read_bytes < iovec_len {
            break;
        }
    }

    total_read as isize
}

const AT_FDCWD: isize = -100;

pub fn sys_openat(dirfd: isize, path: *const u8, flags: u32, mode: u32) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, path) { s } else { return EFAULT.as_isize(); }
    );
    //println!("kernel:pid[{}] tid[{}] sys_openat, dirfd={}, path={}", task.getpid(), task.gettid(), dirfd, path_str);
    //debug!("[kernel] sys_openat: dirfd={}, path={}, flags={}", dirfd, path_str, flags);
    const O_TMPFILE: u32 = 0x400000;
    let (readable, writable) = match flags & 0x3 {
    0x0 => (true, false), // O_RDONLY
    0x1 => (false, true), // O_WRONLY
    0x2 => (true, true),  // O_RDWR
    _ => (true, true),
    };

    // 解析起始目录
    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return EBADF.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    if (flags & O_TMPFILE) != 0 {
        let target_dentry = if let Ok(dentry) = start_dentry.find_tree(&path_str, true) {
        dentry
        } else {
            return ENOENT.as_isize(); // 路径不存在
        };
        
        let stat = target_dentry.inode.get_stat();
        if (stat.mode & 0o170000) != 0o040000 { 
            return ENOTDIR.as_isize(); // 不是目录，报错
        }
        // 3. 将 VfsInode 包装成你的 OSInode / File 结构
        let anon_vfs_inode = Arc::new(TmpfsFileInode::new(0o777));
        let anon_dentry =Dentry::new(
        String::from(""), 
        anon_vfs_inode.clone(), 
        Arc::downgrade(&target_dentry), // 不挂载到全局树
        );
        let anon_file = Arc::new(OSInode::new(
        readable,
        writable,
        false,
        anon_dentry,
        ));

        let files = current_files();
        let mut inner = files.exclusive_access();
        let fd = match inner.alloc_fd(current_nofile_limit()) {
            Some(fd) => fd,
            None => return EMFILE.as_isize(),
        };

        // 4. 塞入进程的文件描述符表
        inner.set_fd(fd, anon_file, FdFlags::from_bits_truncate(flags as usize), flags as usize);
        debug!("kernel:pid[{}] sys_openat: O_TMPFILE success fd={}", task.getpid(), fd);
        
        return fd as isize;
    }
    
    let open_flags = OpenFlags::from_bits_truncate(flags);
    let mask = mode & !current_umask();
    if let Some(inode) = open_file(start_dentry, path_str.as_str(), open_flags, mask) {
        let inode_mode = inode.inode().get_stat().mode;
        let inode_type = inode_mode & S_IFMT;
        if open_flags.should_be_directory() && inode_type != 0o040000 {
            trace!("kernel:pid[{}] VFS: sys_openat failed - '{}' is not a directory", task.getpid(), path_str);
            return ENOTDIR.as_isize(); // 目标文件不是目录
        }

        // Shell redirection opens its target with write access and O_TRUNC.
        // Never let that path treat ext4 directory records as regular data.
        if inode_type == 0o040000 && writable {
            return EISDIR.as_isize();
        }

        if inode_type == 0o100000 && writable && open_flags.contains(OpenFlags::TRUNC) {
            if !inode.truncate(0) {
                return EIO.as_isize();
            }
        }

        let file: Arc<dyn File> = if is_fifo_mode(inode_mode) {
            open_fifo_file(&inode, readable, writable)
        } else {
            inode
        };
        let files = current_files();
        let mut inner = files.exclusive_access();
        let fd = match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
        inner.set_fd(fd, file, FdFlags::from_bits_truncate(flags as usize), flags as usize);
        fd as isize
    } else {
        trace!("kernel:pid[{}] VFS: File '{}' not found", task.getpid(), path_str);
        debug!("kernel:pid[{}] sys_openat: failed path={}", task.getpid(), path_str);
            ENOENT.as_isize()
    }
}

pub fn sys_mknod(dirfd: isize, path: *const u8, mode: u32, _dev: u64) -> isize {
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, path) { s } else { return EFAULT.as_isize(); }
    );

    if path_str.is_empty() {
        return EINVAL.as_isize();
    }

    let start_dentry = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                if (dentry.inode.get_stat().mode & S_IFMT) != 0o040000 {
                    return ENOTDIR.as_isize();
                }
                dentry
            } else {
                return ENOTDIR.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    if start_dentry.find_tree(&path_str, true).is_ok() {
        return EEXIST.as_isize();
    }

    let parent = parent_path(&path_str);
    let         Ok(parent_dentry) = start_dentry.find_tree(&parent, true) else {
        return ENOENT.as_isize();
    };
    if (parent_dentry.inode.get_stat().mode & S_IFMT) != 0o040000 {
        return ENOTDIR.as_isize();
    }

    match mode & S_IFMT {
        crate::fs::S_IFIFO => {
            let name = file_name(&path_str);
            create_fifo_in_dentry(&parent_dentry, name, mode);
            0
        }
        0o100000 => {
            let name = file_name(&path_str);
            create_file_in_dentry(&parent_dentry, name, mode);
            0
        }
        _ => ENOSYS.as_isize(),
    }
}

pub fn sys_close(fd: usize) -> isize {
    let task = current_task().unwrap();
    info!("kernel:pid[{}] sys_close, aim fd = {}", task.getpid(), fd);
    let files = current_files();
    let mut inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let inode = inner.fds[fd].file.as_ref().unwrap().get_stat().ino;
    let file_to_close = inner.fds[fd].file.take();
    inner.clear_fd(fd);
    drop(inner);
    release_record_locks(task.getpid(), inode);
    drop(file_to_close);
    0
}

/// 文件锁，目前是伪实现
pub fn sys_flock(fd: usize, operation: usize) -> isize {
    const LOCK_SH: usize = 1;
    const LOCK_EX: usize = 2;
    const LOCK_NB: usize = 4;
    const LOCK_UN: usize = 8;

    if operation & !(LOCK_SH | LOCK_EX | LOCK_NB | LOCK_UN) != 0
        || !matches!(operation & !LOCK_NB, LOCK_SH | LOCK_EX | LOCK_UN)
    {
        return EINVAL.as_isize();
    }
    let files = current_files();
    let files = files.exclusive_access();
    if fd >= files.fds.len() || files.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    0
}

pub fn sys_accessat(dirfd: isize, path: *const u8, mode: u32, _flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, path) { s } else { return EFAULT.as_isize(); }
    );
    info!("kernel:pid[{}] sys_accessat: dirfd={}, path={}, mode={}", task.getpid(), dirfd, path_str, mode);

    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                // 检查 fd 是否指向目录
                if (dentry.inode.get_stat().mode & 0o170000) != 0o040000 {
                    return ENOTDIR.as_isize();
                }
                dentry
            } else {
                return EBADF.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    if let Some(os_inode) = open_file(start_dentry, path_str.as_str(), OpenFlags::RDONLY, 0) {
        if mode == 0 {
            return 0;
        }
        let stat = os_inode.inode().get_stat();
        let file_mode = stat.mode & 0o777; 
        if (mode & 4) != 0 && (file_mode & 0o444) == 0 {
            return EACCES.as_isize(); 
        }

        if (mode & 2) != 0 && (file_mode & 0o222) == 0 {
            return EACCES.as_isize(); 
        }
        if (mode & 1) != 0 && (file_mode & 0o111) == 0 {
            return EACCES.as_isize(); 
        }
        0 
    }else {
        ENOENT.as_isize() // 文件不存在
    }
}

pub fn sys_pipe(pipe: *mut usize, flags: usize) -> isize {
    warn!("kernel:pid[{}] sys_pipe", current_task().unwrap().getpid());
    let supported_flags = O_CLOEXEC as usize | O_NONBLOCK;
    if flags & !supported_flags != 0 {
        return EINVAL.as_isize();
    }
    let task = current_task().unwrap();
    let token = current_user_token();
    let files = current_files();
    let mut inner = files.exclusive_access();
    let page_table = crate::mm::PageTable::from_token(token);
    let va = pipe as usize;
    if page_table.translate_va(crate::mm::VirtAddr::from(va)).is_none() ||
       page_table.translate_va(crate::mm::VirtAddr::from(va + 4)).is_none() {
        trace!("kernel:pid[{}] sys_pipe error point: {:#x}，", task.getpid(), va);
          return EFAULT.as_isize();
    }
    let (pipe_read, pipe_write) = make_pipe();
    let read_fd = match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
    warn!("kernel:pid[{}] sys_pipe: allocated read_fd={}", task.getpid(), read_fd);
    let fd_flags = if flags & O_CLOEXEC as usize != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };
    let status_flags = flags & O_NONBLOCK;
    inner.set_fd(read_fd, pipe_read, fd_flags, status_flags);
    let write_fd = match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => {
            inner.clear_fd(read_fd);
            return EMFILE.as_isize();
        }
    };
    inner.set_fd(write_fd, pipe_write, fd_flags, O_WRONLY as usize | status_flags);
    // 诊断：打印管道 fd 分配
    warn!("kernel:pid[{}] sys_pipe: allocated write_fd={}", task.getpid(), write_fd);
    // 释放锁，因为下面的write会访问用户锁
    drop(inner);
    // User ABI for pipe is int pipefd[2], i.e. two 32-bit entries.
    let pipe_u32 = pipe as *mut u32;
    if !try_translated_write(token, pipe_u32, read_fd as u32) {
        let mut inner = files.exclusive_access();
        inner.clear_fd(read_fd);
        inner.clear_fd(write_fd);
        return EFAULT.as_isize();
    }
    if !try_translated_write(token, unsafe { pipe_u32.add(1) }, write_fd as u32) {
        let mut inner = files.exclusive_access();
        inner.clear_fd(read_fd);
        inner.clear_fd(write_fd);
        return EFAULT.as_isize();
    }
    //warn!("pipe done");
    0
}

pub fn sys_dup(fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_dup fd = {}", current_task().unwrap().getpid(), fd);
    // warn!("kernel:pid[{}] sys_dup fd = {}", current_task().unwrap().process().pid.0, fd);
    let files = current_files();
    let mut inner = files.exclusive_access();
    // warn!("table len = {}", inner.fd_table.len());
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let new_fd =match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
    // warn!("[kernel] sys_dup: new fd allocated: {}", new_fd);
    let file = Arc::clone(inner.fds[fd].file.as_ref().unwrap());
    let old_status = inner.fds[fd].status;
    inner.set_fd(new_fd, file, FdFlags::empty(), old_status);
    new_fd as isize
}

pub fn sys_lseek(fd: usize, offset: isize, whence: i32) -> isize {
    // warn!("[DEBUG VFS] sys_lseek: fd={}, offset={}, whence={}", fd, offset, whence);
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    
    let file = inner.fds[fd].file.as_ref().unwrap().clone();
    file.lseek(offset, whence)
}
pub fn sys_dup3(fd: usize, new_fd: usize, flags: usize) -> isize {
    const DUP3_ALLOWED_FLAGS: usize = O_CLOEXEC as usize;

    let task = current_task().unwrap();
    trace!("kernel:pid[{}] sys_dup3", task.getpid());
    if flags & !DUP3_ALLOWED_FLAGS != 0 {
        return EINVAL.as_isize();
    }
    if fd == new_fd {
        return EINVAL.as_isize();
    }
    let files = current_files();
    let mut inner = files.exclusive_access();
    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    
    // 不能超过限制
    if new_fd >= current_nofile_limit().min(crate::process::FileDescriptorTable::DEFAULT_LIMIT) {
        return EBADF.as_isize();
    }

    if !ensure_fd_slots(&mut inner, new_fd + 1) {
        return EBADF.as_isize();
    }
    let file = Arc::clone(inner.fds[fd].file.as_ref().unwrap());
    let old_status = inner.fds[fd].status;
    let fd_flags = if flags & O_CLOEXEC as usize != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };
    inner.set_fd(new_fd, file, fd_flags, old_status);
    new_fd as isize
}

pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let token = current_user_token();
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fds[fd].file {
        let file = file.clone();
        drop(inner);
        let mut stat = file.get_stat();
        if let Some(&(asec, ansec, msec, mnsec)) = crate::syscall::fs::TIME_CACHE.lock().get(&stat.ino) {
            stat.atime_sec = asec;
            stat.atime_nsec = ansec;
            stat.mtime_sec = msec;
            stat.mtime_nsec = mnsec;
        }
        if !try_translated_write(token, st, stat) {
            return EFAULT.as_isize();
        }
        0
    } else {
        return EBADF.as_isize();
    }
}
#[repr(C)]
pub struct IoVec {
    pub base: usize, // 指针
    pub len: usize, // 区域长度
}

pub fn sys_writev(fd: usize, iov_ptr: usize, iovcnt: usize) -> isize {
    // 参数合法性检查
    const IOV_MAX: usize = 1024;
    const WRITEV_CHUNK_MAX: usize = 1024 * 1024;
    if iovcnt > IOV_MAX {
        return EINVAL.as_isize();
    }
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    info!("pid[{}] [sys_writev] ENTER fd={}, iov_ptr={:#x}, iovcnt={}", task.getpid(), fd, iov_ptr, iovcnt);
    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let file = inner.fds[fd].file.as_ref().unwrap().clone();
    //file.info_type();
    let status = inner.fds[fd].status;
    let token = current_user_token();
    drop(inner);
    if !file.writable() {
        return EACCES.as_isize();
    }
    if let Some(err) = file.check_write_error() {
        return err.as_isize();
    }
    let nonblock = (status & (O_NONBLOCK | O_NDELAY)) != 0;
    let mut total_written = 0;
    for i in 0..iovcnt {
        let Some(iov_offset) = i.checked_mul(core::mem::size_of::<IoVec>()) else {
            return if total_written == 0 { EFAULT.as_isize() } else { total_written as isize };
        };
        let Some(iov_addr) = iov_ptr.checked_add(iov_offset) else {
            return if total_written == 0 { EFAULT.as_isize() } else { total_written as isize };
        };
        let iovec: IoVec = {
            if let Some(io) = try_translated_read(token, iov_addr as *const IoVec) {
                io
            } else {
                return if total_written == 0 { EFAULT.as_isize() } else { total_written as isize };
            }
        };
        if iovec.len == 0 {
            continue; 
        }
        if !prepare_user_read(token, iovec.base, iovec.len) {
            return if total_written == 0 { EFAULT.as_isize() } else { total_written as isize };
        }
        let mut written_in_iov = 0usize;
        while written_in_iov < iovec.len {
            if nonblock && !file.ready_to_write() {
                return if total_written == 0 {
                    EAGAIN.as_isize()
                } else {
                    total_written as isize
                };
            }
            let chunk_len = (iovec.len - written_in_iov).min(WRITEV_CHUNK_MAX);
            let Some(chunk_base) = iovec.base.checked_add(written_in_iov) else {
                return if total_written == 0 { EFAULT.as_isize() } else { total_written as isize };
            };
            let user_buffer = UserBuffer {
                buffers: translated_byte_buffer(token, chunk_base as *const u8, chunk_len),
            };
            /*if fd == 2 {
                print!("[STDERR PID {}] ", proc.pid.0);
                for buf in &user_buffer.buffers {
                    // 尝试将字节数组转为 UTF-8 字符串
                    if let Ok(s) = core::str::from_utf8(buf) {
                        print!("{}", s);
                    } else {
                        // 如果有无法解析的字符，打印提示
                        print!("<Non-UTF8-Data>"); 
                    }
                }
                println!(" "); // 换行，方便查看
            }*/
            let written = if nonblock {
                match file.write_nonblock(user_buffer) {
                    Ok(written) => written,
                    Err(err) => {
                        return if total_written == 0 {
                            err.as_isize()
                        } else {
                            total_written as isize
                        };
                    }
                }
            } else {
                file.write(user_buffer)
            };
            if written == 0 && chunk_len > 0 {
                if let Some(err) = file.check_write_error() {
                    return if total_written == 0 {
                        err.as_isize()
                    } else {
                        total_written as isize
                    };
                }
                warn!("pid[{}] [sys_writev] FATAL: Underlying file returned 0 on write! fd={}", task.getpid(), fd);
                return total_written as isize;
            }
            total_written += written;
            written_in_iov += written;
            if written < chunk_len {
                return total_written as isize;
            }
        }
    }
    info!("pid[{}] [sys_writev] LEAVE total_written={}", task.getpid(), total_written);
    total_written as isize
}
pub fn sys_statx(dirfd: isize, path: *const u8, flags: u32, mask: u32, st: *mut Statx) -> isize {
    let token = current_user_token();
    let path_str = {
        if let Some(s) = try_translated_str(token, path) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    //println!("kernel:pid[{}] sys_statx: dirfd={}, path={}, flags=0x{:x}, mask=0x{:x}", task.process().pid.0, dirfd, path_str, flags, mask);
    const AT_EMPTY_PATH: u32 = 0x1000;
    if path_str.is_empty() {
        if (flags & AT_EMPTY_PATH) == 0 {
            return EINVAL.as_isize(); // 无效参数 
        }

        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize(); 
        }

        let Some(file) = inner.fds[dirfd as usize].file.as_ref() else {
            return EBADF.as_isize();
        };
        let mut statx_data = if let Some(dentry) = file.get_dentry() {
            dentry.inode.get_statx()
        } else {
            crate::fs::stat_to_statx(file.get_stat())
        };
        // 检查是否有缓存的时间数据，如果有则覆盖 stat 中的时间字段
        if let Some(&(asec, ansec, msec, mnsec)) = TIME_CACHE.lock().get(&statx_data.stx_ino) {
            statx_data.stx_atime.tv_sec = asec;
            statx_data.stx_atime.tv_nsec = ansec as u32;
            statx_data.stx_mtime.tv_sec = msec;
            statx_data.stx_mtime.tv_nsec = mnsec as u32;
        }
        if !try_translated_write(token, st, statx_data) {
            return EFAULT.as_isize();
        }
        return 0;
    }

    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return EBADF.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    let follow_links = (flags & (1 << 8)) == 0; // AT_SYMLINK_NOFOLLOW (0x100)
    if let Ok(target_dentry) = start_dentry.find_tree(&path_str, follow_links) {
        let mut stat = target_dentry.inode.get_statx();
        // 检查是否有缓存的时间数据，如果有则覆盖 stat 中的时间字段
        if let Some(&(asec, ansec, msec, mnsec)) = TIME_CACHE.lock().get(&stat.stx_ino) {
            stat.stx_atime.tv_sec = asec;
            stat.stx_atime.tv_nsec = ansec as u32;
            stat.stx_mtime.tv_sec = msec;
            stat.stx_mtime.tv_nsec = mnsec as u32;
        }

        if !try_translated_write(token, st, stat) {
            return EFAULT.as_isize();
        }
        0
    } else {
        return ENOENT.as_isize(); // 文件不存在
    }
}

pub fn sys_mkdir(path: *const u8, _mode: u32) -> isize {
    let token = current_user_token();

    let path = translate_path(token, path);
    let path = if let Ok(path) = path {
        if path.is_empty() {
            return EINVAL.as_isize(); // 无效路径
        }
        normalize_leading_dot_path(path)
    } else {
        return path.err().unwrap().as_isize();
    };

    debug!("kernel:pid[{}] sys_mkdir: path={}", current_task().unwrap().getpid(), path);
    
    let start = if path.starts_with('/') {
        ROOT_DENTRY.clone()
    } else {
        current_pwd()
    };

    // 目标存在
    if start.find_tree(&path, true).is_ok() {
        return EEXIST.as_isize();
    }

    // 父目录不存在
    let parent = parent_path(&path);
    if start.find_tree(&parent, true).is_err() {
        return ENOENT.as_isize();
    }

    if let Some(_) = make_dir(path.as_str(), _mode) {
        0
    } else {
        EACCES.as_isize() // 权限不足
    }
}

pub fn sys_linkat(
    olddirfd: isize,
    old_name: *const u8,
    newdirfd: isize,
    new_name: *const u8,
    flags: usize,
) -> isize {
    const AT_SYMLINK_FOLLOW: usize = 0x400;
    if flags & !AT_SYMLINK_FOLLOW != 0 {
        return EINVAL.as_isize();
    }

    let token = current_user_token();
    let old_path = match try_translated_str(token, old_name) {
        Some(path) if !path.is_empty() => normalize_leading_dot_path(path),
        Some(_) => return ENOENT.as_isize(),
        None => return EFAULT.as_isize(),
    };
    let new_path = match try_translated_str(token, new_name) {
        Some(path) if !path.is_empty() => normalize_leading_dot_path(path),
        Some(_) => return ENOENT.as_isize(),
        None => return EFAULT.as_isize(),
    };

    let old_base = if old_path.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if olddirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        let Some(file) = (olddirfd >= 0)
            .then(|| inner.fds.get(olddirfd as usize))
            .flatten()
            .and_then(|entry| entry.file.as_ref())
        else {
            return EBADF.as_isize();
        };
        let Some(dentry) = file.get_dentry() else {
            return ENOTDIR.as_isize();
        };
        if (dentry.inode.get_stat().mode & S_IFMT) != 0o040000 {
            return ENOTDIR.as_isize();
        }
        dentry
    };
    let source = match old_base.find_tree(&old_path, (flags & AT_SYMLINK_FOLLOW) != 0) {
        Ok(dentry) => dentry,
        Err(1) => return ENOTDIR.as_isize(),
        Err(_) => return ENOENT.as_isize(),
    };
    if (source.inode.get_stat().mode & S_IFMT) == 0o040000 {
        return EPERM.as_isize();
    }

    let new_base = if new_path.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if newdirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        let Some(file) = (newdirfd >= 0)
            .then(|| inner.fds.get(newdirfd as usize))
            .flatten()
            .and_then(|entry| entry.file.as_ref())
        else {
            return EBADF.as_isize();
        };
        let Some(dentry) = file.get_dentry() else {
            return ENOTDIR.as_isize();
        };
        if (dentry.inode.get_stat().mode & S_IFMT) != 0o040000 {
            return ENOTDIR.as_isize();
        }
        dentry
    };
    let parent = match new_base.find_tree(&parent_path(&new_path), true) {
        Ok(dentry) => dentry,
        Err(1) => return ENOTDIR.as_isize(),
        Err(_) => return ENOENT.as_isize(),
    };
    let name = file_name(&new_path);
    if name.is_empty() {
        return ENOENT.as_isize();
    }
    if parent.find_child(&name).is_some() {
        return EEXIST.as_isize();
    }
    if source.inode.filesystem_kind() != parent.inode.filesystem_kind() {
        return EXDEV.as_isize();
    }
    if !parent.inode.link(&name, source.inode.clone()) {
        warn!("[linkat] pid={} failed {} -> {}", current_task().unwrap().getpid(), old_path, new_path);
        return EIO.as_isize();
    }
    parent.insert(name, source.inode.clone());
    info!("[linkat] pid={} created {} -> {}", current_task().unwrap().getpid(), new_path, old_path);
    0
}

/// 读出符号链接内容（文件本体而非目标）到用户缓冲区，返回实际读出的字节数
pub fn sys_readlinkat(_dirfd: isize, _path: *const u8, _buf: *mut u8, _len: usize) -> isize {
    // 合法性检查
    if _path.is_null() {
        return EFAULT.as_isize();
    }
    if _len == 0 {
        return 0;
    }
    if _buf.is_null() {
        return EFAULT.as_isize();
    }
    // 路径获取
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, _path) { s } else { return EFAULT.as_isize(); }
    );
    if path_str.is_empty() {
        return ENOENT.as_isize();
    }
    debug!("[readlinkat] pid={} dirfd={} path={}", current_task().unwrap().getpid(), _dirfd, path_str);
    // 获取工作路径并查找
    let base_dentry = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if _dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if _dirfd < 0 || _dirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[_dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return ENOTDIR.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };
    // 查找路径对应的 dentry
    let link_dentry = match base_dentry.find_tree(&path_str, false) {
        Ok(d) => d,
        Err(1) => {
            debug!("[readlinkat] path={} failed: ENOTDIR", path_str);
            return ENOTDIR.as_isize();
        }
        Err(_) => {
            debug!("[readlinkat] path={} failed: ENOENT", path_str);
            return ENOENT.as_isize();
        }
    };
    // 文件类型检查
    let st = link_dentry.inode.get_stat();
    let is_symlink = (st.mode & 0o170000) == 0o120000; // 符号链接
    if !is_symlink {
        debug!("[readlinkat] path={} failed: EINVAL (not a symlink)", path_str);
        return EINVAL.as_isize();
    }
    // 读取符号链接内容
    let mut target = alloc::vec![0u8; st.size as usize];
    let read_len = link_dentry.inode.read_at(0, &mut target);
    let copy_len = core::cmp::min(read_len, _len);
    let mut user_bufs = crate::mm::translated_byte_buffer_mut(token, _buf, copy_len);
    let mut copied = 0usize;
    for seg in user_bufs.iter_mut() {
        if copied >= copy_len {
            break;
        }
        let take = core::cmp::min(seg.len(), copy_len - copied);
        seg[..take].copy_from_slice(&target[copied..copied + take]);
        copied += take;
    }
    debug!("[readlinkat] path={} read {} bytes", path_str, copied);
    copied as isize
}

pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> isize {
    //warn!("sys_fcntl fd={}, cmd={}, arg={:#x}", fd, cmd, arg);
    let files = current_files();
    let mut inner = files.exclusive_access();

    let fd_valid = fd < inner.fds.len() && inner.fds[fd].file.is_some();
    if !fd_valid && cmd != F_DUPFD && cmd != F_DUPFD_CLOEXEC {
        return EBADF.as_isize();
    }

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            if !fd_valid {
                return EBADF.as_isize();
            }
            // 防溢出
            let nofile_limit = current_nofile_limit()
                .min(crate::process::FileDescriptorTable::DEFAULT_LIMIT);
            if arg >= nofile_limit {
                return EBADF.as_isize();
            }
            let mut new_fd = arg;
            while new_fd < inner.fds.len() {
                if inner.fds[new_fd].file.is_none() {
                    break;
                }
                new_fd += 1;
            }
            // 再次检查
            if new_fd >= nofile_limit {
                return EBADF.as_isize();
            }
            if !ensure_fd_slots(&mut inner, new_fd + 1) {
                return EBADF.as_isize();
            }
            let file = Arc::clone(inner.fds[fd].file.as_ref().unwrap());
            let old_status = inner.fds[fd].status;
            inner.set_fd(
                new_fd,
                file,
                if cmd == F_DUPFD_CLOEXEC { FdFlags::CLOEXEC } else { FdFlags::empty() },
                old_status,
            );
            new_fd as isize
        }
        F_GETFD => {
            if inner.fds[fd].flags.contains(FdFlags::CLOEXEC) { FD_CLOEXEC as isize } else { 0 }
        }
        F_SETFD => {
            if (arg & FD_CLOEXEC) != 0 {
                inner.fds[fd].flags.insert(FdFlags::CLOEXEC);
            } else {
                inner.fds[fd].flags.remove(FdFlags::CLOEXEC);
            }
            0
        }
        F_GETFL => inner.fds[fd].status as isize,
        F_SETFL => {
            let old = inner.fds[fd].status;
            inner.fds[fd].status = (old & O_ACCMODE) | (arg & !O_ACCMODE);
            0
        }
        F_GETLK | F_SETLK | F_SETLKW => {
            let inode = inner.fds[fd].file.as_ref().unwrap().get_stat().ino;
            let pid = current_task().unwrap().getpid();
            drop(inner);
            let Some(mut flock) = try_translated_read(current_user_token(), arg as *const Flock) else {
                return EFAULT.as_isize();
            };
            let Some((start, len)) = flock_range(&flock) else {
                return EINVAL.as_isize();
            };
            if !matches!(flock.l_type, F_RDLCK | F_WRLCK | F_UNLCK) {
                return EINVAL.as_isize();
            }

            let mut locks = RECORD_LOCKS.exclusive_access();
            let entries = locks.entry(inode).or_insert_with(Vec::new);
            let conflict = entries.iter().find(|entry| {
                entry.owner_pid != pid
                    && (entry.lock_type == F_WRLCK || flock.l_type == F_WRLCK)
                    && ranges_overlap(entry.start, entry.len, start, len)
            }).copied();

            if cmd == F_GETLK {
                if let Some(entry) = conflict {
                    flock.l_type = entry.lock_type;
                    flock.l_whence = SEEK_SET;
                    flock.l_start = entry.start as i64;
                    flock.l_len = entry.len.unwrap_or(0) as i64;
                    flock.l_pid = entry.owner_pid as i32;
                } else {
                    flock.l_type = F_UNLCK;
                    flock.l_pid = 0;
                }
                drop(locks);
                return if try_translated_write(current_user_token(), arg as *mut Flock, flock) {
                    0
                } else {
                    EFAULT.as_isize()
                };
            }

            if flock.l_type == F_UNLCK {
                entries.retain(|entry| {
                    entry.owner_pid != pid || !ranges_overlap(entry.start, entry.len, start, len)
                });
                if entries.is_empty() {
                    locks.remove(&inode);
                }
                return 0;
            }
            if conflict.is_some() {
                return EAGAIN.as_isize();
            }
            entries.retain(|entry| {
                entry.owner_pid != pid || !ranges_overlap(entry.start, entry.len, start, len)
            });
            entries.push(RecordLock {
                owner_pid: pid,
                lock_type: flock.l_type,
                start,
                len,
            });
            0
        }
        F_GETPIPE_SIZE => {
            if inner.fds[fd].file.as_ref().unwrap().get_stat().mode & S_IFMT != crate::fs::S_IFIFO {
                return EBADF.as_isize();
            }
            16*4096 as isize
        }
        _ => EINVAL.as_isize(),
    }
}



/// YOUR JOB: Implement unlinkat.
pub fn sys_unlinkat(dirfd: isize, path: *const u8, flags: usize) -> isize {
    // 校验 flags：只允许 0 或 AT_REMOVEDIR
    if flags != 0 && flags != AT_REMOVEDIR {
        return EINVAL.as_isize();
    }
    let token = current_user_token();
    let path_str = {
        if let Some(s) = try_translated_str(token, path) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    if path_str.len() == 0 {
        return ENOENT.as_isize();
    }
    if path_str.len() > 255 {
        return ENAMETOOLONG.as_isize();
    }
    trace!("kernel:pid[{}] sys_unlinkat dirfd={} path={} flags={:#x}", current_task().unwrap().getpid(), dirfd, path_str, flags);

    let task = current_task().unwrap();
    let base_dir = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || (dirfd as usize) >= inner.fds.len() || inner.fds[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        // 确保 dirfd 指向的是一个目录，否则返回 ENOTDIR
        let file = inner.fds[dirfd as usize].file.as_ref().unwrap().clone();
        if let Some(dentry) = file.get_dentry() {
            if (dentry.inode.get_stat().mode & 0o170000) != 0o040000 {
                return ENOTDIR.as_isize();
            }
            dentry
        } else {
            return ENOTDIR.as_isize();
        }
    };

    // 找到目标文件的 dentry
    let target_dentry = base_dir.find_tree(&path_str, false);
    let target = match target_dentry {
        Ok(d) => d,
        Err(1) => return ENOTDIR.as_isize(),
        Err(_) => return ENOENT.as_isize(),
    };
    let stat = target.inode.get_stat();
    let is_dir = (stat.mode & 0o040000) != 0; // 判断是否为目录
    // 类型检查
    let removing_dir = (flags & AT_REMOVEDIR) != 0;
    if is_dir && !removing_dir { return EISDIR.as_isize();}
    if !is_dir && removing_dir { return ENOTDIR.as_isize();}
    // 找到上级目录
    let parent_path_str = parent_path(&path_str);
    let name = file_name(&path_str);
    let parent_dentry = base_dir.find_tree(&parent_path_str, true);
    if let Ok(parent) = parent_dentry {
        // 检查父目录权限：
        // - 写权限(w)：unlink 本质是修改目录条目
        // - 执行权限(x)：必须能 search 进入该目录
        let parent_stat = parent.inode.get_stat();
        let parent_mode = FileMode::from_bits_truncate(parent_stat.mode as u16);
        if !parent_mode.contains(FileMode::U_WRITE) || !parent_mode.contains(FileMode::U_EXECUTE) {
            return EACCES.as_isize();
        }
        let _namespace_guard = parent.namespace_lock.lock();
        // 尝试删除
        if let Some(_inode_id) = parent.inode.delete_dir_entry(&name) {
            if removing_dir {
                // Drop the directory's implicit '.' link and the parent's
                // link contributed by the child's implicit '..'.
                target.inode.dec_link_count();
                parent.inode.dec_link_count();
            }
            parent.children.lock().remove(&name);
            return 0;
        } else {
            // 驱动引起的删除不成功
            error!("kernel:pid[{}] VFS failed to delete '{}'. Underlay FS returned None.", task.getpid(), name);
            return EACCES.as_isize();
        }
    }
    ENOTDIR.as_isize() // 父目录不存在
}
pub fn sys_sendfile(out_fd: usize, in_fd: usize, _offset_ptr: usize, count: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_sendfile: out_fd={}, in_fd={}, count={}",
        current_task().unwrap().getpid(),
        out_fd,
        in_fd,
        count
    );
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    // 文件描述符合法性检查
    if out_fd >= inner.fds.len() || in_fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    let out_file = match &inner.fds[out_fd].file {
        Some(file) => file.clone(),
        None => return EBADF.as_isize(),
    };
    let in_file = match &inner.fds[in_fd].file {
        Some(file) => file.clone(),
        None => return EBADF.as_isize(),
    };
    if !out_file.writable() || !in_file.readable() {
        return EBADF.as_isize();
    }
    // For pipe-based shell pipelines, use userspace read/write fallback.
    // The fallback is slower but avoids multicore stalls from long in-kernel transfers.
    let out_mode = out_file.get_stat().mode & 0o170000;
    let in_mode = in_file.get_stat().mode & 0o170000;
    if out_mode == 0o010000 || in_mode == 0o010000 {
        return ENOSYS.as_isize();
    }
    
    drop(inner);

    // 缓冲区
    let mut buffer = [0u8; 4096];
    let mut total_transferred = 0;

    while total_transferred < count {
        let remain = count - total_transferred;
        let read_len = remain.min(buffer.len());
        let read_slice = unsafe {
            core::slice::from_raw_parts_mut(buffer.as_mut_ptr(), read_len)
        };
        let user_buf = UserBuffer { buffers: vec![read_slice] };
        let read_bytes = in_file.read(user_buf);
        if read_bytes == 0 {
            break; // 读到底了
        }

        let mut written_so_far = 0;
        while written_so_far < read_bytes {
            let write_len = read_bytes - written_so_far;

            let write_slice = unsafe {
                core::slice::from_raw_parts_mut(
                    buffer.as_mut_ptr().add(written_so_far), 
                    write_len
                )
            };
            let w_user_buf = UserBuffer { buffers: vec![write_slice] };

            let write_bytes = out_file.write(w_user_buf);
            if write_bytes == 0 {
                if total_transferred == 0 { return EBADF.as_isize(); } else { break; }
            }
            written_so_far += write_bytes;
        }

        total_transferred += written_so_far;
    }

    trace!("kernel:pid[{}] sys_sendfile: transferred={}", task.getpid(), total_transferred);
    total_transferred as isize
}

pub fn sys_copy_file_range(
    fd_in: usize,
    off_in: *mut i64,
    fd_out: usize,
    off_out: *mut i64,
    len: usize,
    flags: u32,
) -> isize {
    const COPY_CHUNK: usize = 64 * 1024;

    if flags != 0 {
        return EINVAL.as_isize();
    }
    if len == 0 {
        return 0;
    }

    let files = current_files();
    let inner = files.exclusive_access();
    if fd_in >= inner.fds.len() || fd_out >= inner.fds.len() {
        return EBADF.as_isize();
    }
    let Some(file_in) = inner.fds[fd_in].file.as_ref().cloned() else {
        return EBADF.as_isize();
    };
    let Some(file_out) = inner.fds[fd_out].file.as_ref().cloned() else {
        return EBADF.as_isize();
    };
    drop(inner);

    if !file_in.readable() || !file_out.writable() {
        return EBADF.as_isize();
    }

    let token = current_user_token();
    let mut input_offset = if off_in.is_null() {
        None
    } else {
        match try_translated_read(token, off_in as *const i64) {
            Some(offset) if offset >= 0 => Some(offset as usize),
            Some(_) => return EINVAL.as_isize(),
            None => return EFAULT.as_isize(),
        }
    };
    let mut output_offset = if off_out.is_null() {
        None
    } else {
        match try_translated_read(token, off_out as *const i64) {
            Some(offset) if offset >= 0 => Some(offset as usize),
            Some(_) => return EINVAL.as_isize(),
            None => return EFAULT.as_isize(),
        }
    };

    let mut copied = 0usize;
    while copied < len {
        let chunk_len = (len - copied).min(COPY_CHUNK);
        let mut buffer = alloc::vec![0; chunk_len];
        let read_slice = unsafe {
            core::slice::from_raw_parts_mut(buffer.as_mut_ptr(), chunk_len)
        };
        let read_buffer = UserBuffer {
            buffers: vec![read_slice],
        };
        let read_len = match input_offset {
            Some(offset) => file_in.read_at(offset, read_buffer),
            None => file_in.read(read_buffer),
        };
        if read_len == 0 {
            break;
        }

        let mut written = 0usize;
        while written < read_len {
            let write_slice = unsafe {
                core::slice::from_raw_parts_mut(
                    buffer.as_mut_ptr().add(written),
                    read_len - written,
                )
            };
            let write_buffer = UserBuffer {
                buffers: vec![write_slice],
            };
            let write_len = match output_offset {
                Some(offset) => file_out.write_at(offset + written, write_buffer),
                None => file_out.write(write_buffer),
            };
            if write_len == 0 {
                break;
            }
            written += write_len;
        }

        if written == 0 {
            break;
        }
        copied += written;
        if let Some(offset) = input_offset.as_mut() {
            *offset += written;
        }
        if let Some(offset) = output_offset.as_mut() {
            *offset += written;
        }
        if written < read_len {
            break;
        }
    }

    if let Some(offset) = input_offset {
        if !try_translated_write(token, off_in, offset as i64) {
            return if copied == 0 { EFAULT.as_isize() } else { copied as isize };
        }
    }
    if let Some(offset) = output_offset {
        if !try_translated_write(token, off_out, offset as i64) {
            return if copied == 0 { EFAULT.as_isize() } else { copied as isize };
        }
    }

    copied as isize
}

pub fn sys_getdents(fd: usize, dirp: *mut u8, count: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fds[fd].file {
        let file = file.clone();
        drop(inner);
        if !file.readable() {
            return EACCES.as_isize(); // 权限不足
        }
        trace!("kernel:pid[{}] sys_getdents: fd={}, count={}", task.getpid(), fd, count);
        let mut bufs = translated_byte_buffer(token, dirp, count);
        if bufs.is_empty() {
            return EFAULT.as_isize();
        }

        // The user buffer can cross page boundaries.  Stage each VFS read in
        // a contiguous buffer, but never ask the VFS for more data than we
        // can copy back to userspace in this syscall.
        const CHUNK: usize = 32768;
        let user_len = core::cmp::min(
            count,
            bufs.iter().fold(0usize, |len, buf| len.saturating_add(buf.len())),
        );
        if user_len == 0 {
            return EFAULT.as_isize();
        }
        let mut chunk = alloc::vec![0u8; core::cmp::min(CHUNK, user_len)];
        let mut seg_idx = 0usize;
        let mut seg_off = 0usize;
        let mut total: usize = 0;
        while total < user_len {
            let request_len = core::cmp::min(chunk.len(), user_len - total);
            let n = file.getdents(&mut chunk[..request_len]);
            if n <= 0 {
                if total == 0 {
                    return n;
                }
                break;
            }
            let n = n as usize;
            if n > request_len {
                // A VFS implementation must not return more than its input
                // buffer.  Returning such a length would expose uninitialised
                // userspace bytes as directory records.
                return EIO.as_isize();
            }
            let mut copied = 0usize;
            while copied < n && seg_idx < bufs.len() {
                if seg_off == bufs[seg_idx].len() {
                    seg_idx += 1;
                    seg_off = 0;
                    continue;
                }
                let c = core::cmp::min(n - copied, bufs[seg_idx].len() - seg_off);
                bufs[seg_idx][seg_off..seg_off + c].copy_from_slice(&chunk[copied..copied + c]);
                copied += c;
                seg_off += c;
            }
            if copied != n {
                return EFAULT.as_isize();
            }
            total += copied;
            if total == user_len {
                break;
            }
        }
        total as isize
    } else {
        return EBADF.as_isize();
    }
}

pub fn sys_getcwd(buf: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let path = current_pwd().get_full_path();

    let path_bytes = path.as_bytes();
    if path_bytes.len() + 1 > size {
        return ENAMETOOLONG.as_isize();
    }
    let mut user_buf = UserBuffer::new(crate::mm::translated_byte_buffer_mut(token, buf, size));
    let mut current_offset = 0;
    let mut path_vec = path_bytes.to_vec();
    path_vec.push(0);
    for buffer in user_buf.buffers.iter_mut() {
        let copy_len = buffer.len().min(path_vec.len() - current_offset);
        buffer[..copy_len].copy_from_slice(&path_vec[current_offset..current_offset + copy_len]);
        current_offset += copy_len;
        if current_offset == path_vec.len() {
            break;
        }
    }
    path_bytes.len() as isize
}

pub fn sys_chdir(path: *const u8) -> isize {
    let token = current_user_token();
    let path_str = try_translated_str(token, path);
    
    let path_str = if let Some(path_str) = path_str {
        if path_str.len() > 255 {
            return ENAMETOOLONG.as_isize();
        }
        normalize_leading_dot_path(path_str)
    } else {
        return EFAULT.as_isize();
    };

    debug!("kernel:pid[{}] sys_chdir: path={}", current_task().unwrap().getpid(), path_str);
    
    let task = current_task().unwrap();
    let fs = task.inner_exclusive_access().fs.clone();
    let cwd = fs.exclusive_access().get_pwd();
    let current_path = cwd.get_full_path();

    let full_path = if path_str.starts_with('/') {
        path_str // 绝对路径
    } else {
        // 相对路径，拼接 CWD
        let mut p = current_path;
        if !p.ends_with('/') {
            p.push('/');
        }
        p.push_str(&path_str);
        p
    };

    if let Some(inode) = open_file(cwd, full_path.as_str(), OpenFlags::DIRECTORY,0) {
        fs.exclusive_access().set_pwd(inode.get_dentry());
        0
    } else {
        ENOENT.as_isize() // 目录不存在
    }
}

pub fn sys_fchdir(fd: usize) -> isize {
    let file = {
        let files = current_files();
        let inner = files.exclusive_access();
        if fd >= inner.fds.len() {
            return EBADF.as_isize();
        }
        let Some(file) = inner.fds[fd].file.as_ref() else {
            return EBADF.as_isize();
        };
        file.clone()
    };

    let Some(dentry) = file.get_dentry() else {
        return ENOTDIR.as_isize();
    };
    if dentry.inode.get_stat().mode & S_IFMT != 0o040000 {
        return ENOTDIR.as_isize();
    }

    let task = current_task().unwrap();
    let fs = task.inner_exclusive_access().fs.clone();
    fs.exclusive_access().set_pwd(dentry);
    0
}

pub fn sys_fadvise64(fd: usize, _offset: usize, _len: usize, advice: i32) -> isize {
    const POSIX_FADV_NORMAL: i32 = 0;
    const POSIX_FADV_RANDOM: i32 = 1;
    const POSIX_FADV_SEQUENTIAL: i32 = 2;
    const POSIX_FADV_WILLNEED: i32 = 3;
    const POSIX_FADV_DONTNEED: i32 = 4;
    const POSIX_FADV_NOREUSE: i32 = 5;

    let file = {
        let files = current_files();
        let inner = files.exclusive_access();
        if fd >= inner.fds.len() {
            return EBADF.as_isize();
        }
        let Some(file) = inner.fds[fd].file.as_ref() else {
            return EBADF.as_isize();
        };
        file.clone()
    };

    if !matches!(
        advice,
        POSIX_FADV_NORMAL
            | POSIX_FADV_RANDOM
            | POSIX_FADV_SEQUENTIAL
            | POSIX_FADV_WILLNEED
            | POSIX_FADV_DONTNEED
            | POSIX_FADV_NOREUSE
    ) {
        return EINVAL.as_isize();
    }
    if file.as_any().is::<crate::fs::Pipe>() {
        return ESPIPE.as_isize();
    }

    // 最简兼容实现：接受合法提示，但暂不调整预读或页缓存策略。
    0
}

/// 挂载，目前是伪实现
pub fn sys_mount(source: *const u8, target: *const u8, filesystemtype: *const u8, mountflags: u32) -> isize {
    let token = current_user_token();
    let source_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, source) { s } else { return EFAULT.as_isize(); }
    );
    let target_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, target) { s } else { return EFAULT.as_isize(); }
    );
    let filesystemtype_str = {
        if let Some(s) = try_translated_str(token, filesystemtype) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    return 0;
}

/// 取消挂载，目前是伪实现
pub fn sys_umount(target: *const u8) -> isize {
    let token = current_user_token();
    let target_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, target) { s } else { return EFAULT.as_isize(); }
    );
    return 0;
}

/// 移除，目前是伪实现
pub fn sys_fremovexattr(_fd: isize, _name: *const u8) -> isize {
    let name_str = if _name.is_null() {
        String::new()
    } else {
        let token = current_user_token();
        if let Some(s) = try_translated_str(token, _name) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    return 0; // 目前不支持扩展属性，直接返回成功
}

pub fn sys_fstatat(dirfd: isize, path_ptr: *const u8, st: *mut Stat, flags: usize) -> isize {
    let token = current_user_token();
    let path_str = {
        if let Some(s) = crate::mm::try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    const AT_SYMLINK_NOFOLLOW: usize = 0x100;
    const AT_EMPTY_PATH: usize = 0x1000;
    let follow_links = (flags & AT_SYMLINK_NOFOLLOW) == 0;

    let cwd = current_pwd();
    //空路径查找文件描述符
    if path_str.is_empty() {
        if (flags & AT_EMPTY_PATH) == 0 {
            return ENOENT.as_isize();
        }
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || (dirfd as usize) >= inner.fds.len() {
            return EBADF.as_isize();
        }
        let Some(file) = &inner.fds[dirfd as usize].file else {
            return EBADF.as_isize();
        };
        let file = file.clone();
        let mut stat = file.get_stat();
        if let Some(&(asec, ansec, msec, mnsec)) = TIME_CACHE.lock().get(&stat.ino) {
            stat.atime_sec = asec;
            stat.atime_nsec = ansec;
            stat.mtime_sec = msec;
            stat.mtime_nsec = mnsec;
        }
        if !try_translated_write(token, st, stat) {
            return EFAULT.as_isize();
        }
        return 0;
    }
    
    if path_str.contains("Zone.Identifier") {
        let mut stat: Stat = unsafe { core::mem::zeroed() };
        stat.mode = 0o100755; // 假装它是个普通空文件，让 du 闭嘴
        stat.size = 0;
        crate::mm::translated_write(token, st, stat);
        return 0;
    }
    let base_dir = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        cwd.clone()
    } else {
        // 相对路径 + 指定的目录 fd
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || (dirfd as usize) >= inner.fds.len() || inner.fds[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        let file = inner.fds[dirfd as usize].file.as_ref().unwrap().clone();
        drop(inner);
        if let Some(dentry) = file.get_dentry() {
            if (dentry.inode.get_stat().mode & 0o170000) != 0o040000 {
                return ENOTDIR.as_isize();
            }
            dentry
        } else {
            return ENOTDIR.as_isize();
        }
    };

    // 查找文件（follow_links: stat 跟随符号链接，lstat 不跟随）
    let target_dentry = base_dir.find_tree(&path_str, follow_links);

    match target_dentry {
        Ok(dentry) => {
            let stat = dentry.inode.get_stat();
            //warn!("mtime = {}.{} , atime = {}.{}, inode={}", stat.mtime_sec, stat.mtime_nsec, stat.atime_sec, stat.atime_nsec, dentry.name);
            if !try_translated_write(token, st, stat) {
                return EFAULT.as_isize();
            }
            0
        }
        Err(0) => {
            // 符号链接循环
            ELOOP.as_isize()
        }
        Err(1) => {
            // 中间路径不是目录
            ENOTDIR.as_isize()
        }
        Err(_) => {
            // 路径不存在
            ENOENT.as_isize()
        }
    }
}

pub fn sys_pread64(fd: usize, buf: *mut u8, count: usize, offset: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fds[fd].file {
        let file = file.clone();
        drop(inner);
        if !file.readable() {
            return EACCES.as_isize();
        }
        trace!("kernel:pid[{}] sys_pread64: fd={}, count={}, offset={}", task.getpid(), fd, count, offset);
        file.pread(offset, UserBuffer::new(translated_byte_buffer(token, buf, count))) as isize
    } else {
        EBADF.as_isize()
    }
}

/// 在指定偏移量写入，即 write_at
pub fn sys_pwrite64(fd: usize, buf: *const u8, count: usize, offset: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fds[fd].file {
        let file = file.clone();
        drop(inner);
        if !file.writable() {
            return EACCES.as_isize();
        }
        trace!("kernel:pid[{}] sys_pwrite64: fd={}, count={}, offset={}", task.getpid(), fd, count, offset);
        file.write_at(offset, UserBuffer::new(translated_byte_buffer(token, buf as *mut u8, count))) as isize
    } else {
        EBADF.as_isize()
    }
}

/// 修改权限模式
pub fn sys_fchmodat(dirfd: isize, path_ptr: *const u8, mode: u32) -> isize {
    let token = current_user_token();
    let euid = current_euid();
    
    let path = {
        if let Some(s) = try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };

    let base_dir = if path.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || (dirfd as usize) >= inner.fds.len() || inner.fds[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        // 从 dirfd 对应的目录开始查找
        if let Some(dentry) = inner.fds[dirfd as usize].file.as_ref().and_then(|f| f.get_dentry()) {
            dentry
        } else {
            return EBADF.as_isize();
        }
    };
    match base_dir.find_tree(path.as_str(), true) {
        Ok(dentry) => {
            let mut perm = dentry.inode.get_perm();
            // 鉴权：仅root或所有者可以修改
            if euid != 0 && perm.uid != euid {
                return EPERM.as_isize();
            }
            
            // 仅修改(特殊)权限位
            let new_mode = (perm.mode.bits() & !0o7777) | (mode as u16 & 0o7777);
            perm.set_mode(crate::auth::FileMode::from_bits_truncate(new_mode));
            
            if dentry.inode.set_perm(perm) {
                0
            } else {
                EACCES.as_isize() 
            }
        }
        Err(1) => {
            ENOTDIR.as_isize()
        }
        Err(_) => {
            ENOENT.as_isize()
        }
    }
}

/// 通过 fd 修改文件权限
/// Linux: int fchmod(int fd, mode_t mode)
pub fn sys_fchmod(fd: usize, mode: u32) -> isize {
    let euid = current_euid();
    let files = current_files();
    let inner = files.exclusive_access();

    if fd >= inner.fds.len() || inner.fds[fd].file.is_none() {
        return EBADF.as_isize();
    }

    let file = inner.fds[fd].file.as_ref().unwrap().clone();
    drop(inner);

    let dentry = match file.get_dentry() {
        Some(d) => d,
        None => return EBADF.as_isize(),
    };

    let mut perm = dentry.inode.get_perm();
    // 鉴权：仅root或所有者可以修改
    if euid != 0 && perm.uid != euid {
        return EPERM.as_isize();
    }

    // 仅修改权限位（低12位）
    let new_mode = (perm.mode.bits() & !0o7777) | (mode as u16 & 0o7777);
    perm.set_mode(crate::auth::FileMode::from_bits_truncate(new_mode));

    if dentry.inode.set_perm(perm) {
        0
    } else {
        EACCES.as_isize()
    }
}

/// 通过 fd 修改文件所有者和组。
/// Linux: int fchown(int fd, uid_t owner, gid_t group)
pub fn sys_fchown(fd: usize, owner: u32, group: u32) -> isize {
    let euid = current_euid();
    let files = current_files();
    let inner = files.exclusive_access();

    let Some(file) = inner.fds.get(fd).and_then(|entry| entry.file.as_ref()).cloned() else {
        return EBADF.as_isize();
    };
    drop(inner);

    let Some(dentry) = file.get_dentry() else {
        return EBADF.as_isize();
    };

    let mut perm = dentry.inode.get_perm();
    let requested_owner = (owner != u32::MAX).then_some(owner);
    let requested_group = (group != u32::MAX).then_some(group);

    if euid != 0 {
        if requested_owner.is_some_and(|requested| requested != perm.uid)
            || (requested_group.is_some() && perm.uid != euid)
        {
            return EPERM.as_isize();
        }
    }

    if let Some(owner) = requested_owner {
        perm.set_uid(owner);
    }
    if let Some(group) = requested_group {
        perm.set_gid(group);
    }

    if dentry.inode.set_perm(perm) {
        0
    } else {
        EACCES.as_isize()
    }
}

/// 修改所有者/组
pub fn sys_fchownat(dirfd: isize, path_ptr: *const u8, owner: u32, group: u32) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let euid = current_euid();
    info!("pid[{}] sys_fchownat: dirfd={}, owner={}, group={}", task.getpid(), dirfd, owner, group);
    let path = {
        if let Some(s) = try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    info!("pid[{}] sys_fchownat: path '{}'", task.getpid(), path);

    let base_dir = if path.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if dirfd < 0 || (dirfd as usize) >= inner.fds.len() || inner.fds[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        if let Some(dentry) = inner.fds[dirfd as usize].file.as_ref().and_then(|f| f.get_dentry()) {
            dentry
        } else {
            return EBADF.as_isize();
        }
    };
    match base_dir.find_tree(path.as_str(), true) {
        Ok(dentry) => {
            let mut perm = dentry.inode.get_perm();
            // owner/group 参数为 0xffffffff 表示不修改对应项
            let req_owner = if owner == 0xffffffff { None } else { Some(owner) };
            let req_group = if group == 0xffffffff { None } else { Some(group) };
            
            // 鉴权
            if euid != 0 { // root跳过鉴权
                // 检查是否是所有者
                if let Some(o) = req_owner {
                    if o != perm.uid { return super::errno::Errno::EPERM.as_isize(); }
                }
                if req_group.is_some() && perm.uid != euid {
                    return EPERM.as_isize();
                }
            }

            if let Some(o) = req_owner { perm.set_uid(o); }
            if let Some(g) = req_group { perm.set_gid(g); }

            if dentry.inode.set_perm(perm) {
                0
            } else {
                EACCES.as_isize()
            }
        }
        Err(1) => {
            ENOTDIR.as_isize()
        }
        Err(_) => {
            ENOENT.as_isize()
        }
    }
}

/// 预分配文件空间
/// Linux: int fallocate(int fd, int mode, off_t offset, off_t len)
pub fn sys_fallocate(fd: usize, mode: usize, offset: i64, len: i64) -> isize {
    const FALLOC_FL_KEEP_SIZE: usize = 0x01;

    let files = current_files();
    let inner = files.exclusive_access();

    if fd >= inner.fds.len() {
        return EBADF.as_isize();
    }

    if let Some(file) = &inner.fds[fd].file {
        if !file.writable() {
            return EBADF.as_isize();
        }
        // 仅支持 mode=0 和 FALLOC_FL_KEEP_SIZE
        if mode != 0 && mode != FALLOC_FL_KEEP_SIZE {
            return EOPNOTSUPP.as_isize();
        }
        drop(inner);

        if offset < 0 || len <= 0 {
            return EINVAL.as_isize();
        }
        // 溢出检查
        if let Some(end) = offset.checked_add(len) {
            if end < 0 {
                return EFBIG.as_isize();
            }
        } else {
            return EFBIG.as_isize();
        }

        // tmpfs/ext4 按需分配，预分配为空操作
        return 0;
    }

    EBADF.as_isize()
}

// memfd flags
bitflags::bitflags! {
    pub struct MemfdFlags: u32 {
        const MFD_CLOEXEC       = 0x0001;
        const MFD_ALLOW_SEALING = 0x0002;
        const MFD_HUGETLB       = 0x0004;
        const MFD_NOEXEC_SEAL   = 0x0008;
        const MFD_EXEC          = 0x0010;
        
        // 巨页大页掩码
        const MFD_HUGE_MASK     = 0x3f << 26;
        
        // 常见的巨页大小
        const MFD_HUGE_64KB     = 16 << 26;
        const MFD_HUGE_512KB    = 19 << 26;
        const MFD_HUGE_1MB      = 20 << 26;
        const MFD_HUGE_2MB      = 21 << 26;
        const MFD_HUGE_8MB      = 23 << 26;
        const MFD_HUGE_16MB     = 24 << 26;
        const MFD_HUGE_32MB     = 25 << 26;
        const MFD_HUGE_256MB    = 28 << 26;
        const MFD_HUGE_512MB    = 29 << 26;
        const MFD_HUGE_1GB      = 30 << 26;
        const MFD_HUGE_2GB      = 31 << 26;
        const MFD_HUGE_16GB     = 34 << 26;
    }
}

/// 创建匿名内存文件
pub fn sys_memfd_create(name: *const u8, flags: u32) -> isize {
    let flags = match MemfdFlags::from_bits(flags) {
        Some(f) => f,
        None => return EINVAL.as_isize(),
    };

    // 解析页大小
    let page_size = {
        if flags.contains(MemfdFlags::MFD_HUGETLB) {
            match flags.intersection(MemfdFlags::MFD_HUGE_MASK) {
                // 不支持的巨页大小
                MemfdFlags::MFD_HUGE_64KB | MemfdFlags::MFD_HUGE_512KB | MemfdFlags::MFD_HUGE_1MB | 
                MemfdFlags::MFD_HUGE_8MB | MemfdFlags::MFD_HUGE_16MB |
                MemfdFlags::MFD_HUGE_32MB | MemfdFlags::MFD_HUGE_256MB | MemfdFlags::MFD_HUGE_512MB |
                MemfdFlags::MFD_HUGE_2GB | MemfdFlags::MFD_HUGE_16GB => {
                    return ENODEV.as_isize(); // 不支持的巨页大小返回没有设备
                },
                MemfdFlags::MFD_HUGE_2MB => PageSize::Page2M,
                MemfdFlags::MFD_HUGE_1GB => PageSize::Page1G,
                _ => return EINVAL.as_isize(),
            }
        } else {
            PageSize::Page4K
        }
    };

    let task = current_task().unwrap();
    let token = current_user_token();
    let name_str = {
        if let Some(s) = try_translated_str(token, name) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };

    // 创建 memfd 文件（只创建 inode + dentry，不映射 VPN）
    let file = crate::fs::memfd::create_memfd(&name_str, page_size);

    let files = current_files();
    let mut inner = files.exclusive_access();
    let fd = match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => return EMFILE.as_isize(),
    };
    inner.set_fd(fd, file, if flags.contains(MemfdFlags::MFD_CLOEXEC) { FdFlags::CLOEXEC } else { FdFlags::empty() }, 0);

    trace!("kernel:pid[{}] sys_memfd_create: name='{}', page_size={:?}, fd={}",
        task.getpid(), name_str, page_size, fd);
    fd as isize
}

pub fn sys_vmsplice(fd: usize, iov: *const IoVec, iovcnt: usize, flags: u32) -> isize {
    //warn!("fd={}, iov={:?}, iovcnt={}, flags=0x{:x}", fd, iov, iovcnt, flags);
    const IOV_MAX: usize = 1024;
    const IOV_BUF_MAX: usize = 1024 * 1024; // 1 MiB per iovec element
    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;
    const SPLICE_FLAGS_MASK: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;

    if (flags & !SPLICE_FLAGS_MASK) != 0 {
        return EINVAL.as_isize();
    }
    if iovcnt > IOV_MAX {
        return EINVAL.as_isize();
    }
    if iovcnt > 0 && iov.is_null() {
        return EFAULT.as_isize();
    }

    let Some(task) = current_task() else {
        return ENOSYS.as_isize();
    };
    let token = current_user_token();

    let files = current_files();
    let inner = files.exclusive_access();
    if fd >= inner.fds.len() {
        warn!("vmsplice target fd {} out of range", fd);
        return EBADF.as_isize();
    }

    let fd_entry = &inner.fds[fd];
    let file = match fd_entry.file.as_ref() {
        Some(f) => f.clone(),
        None => {
            warn!("vmsplice target fd {} has no associated file", fd);
            return EBADF.as_isize();
        }
    };
    let fd_status = fd_entry.status;
    drop(inner);

    // vmsplice(..., fd, ...) writes user iov into a pipe.
    if !file.writable() && !file.readable() {
        warn!("vmsplice target fd {} is not writable", fd);
        return EBADF.as_isize();
    }
    if (file.get_stat().mode & S_IFMT) != crate::fs::S_IFIFO {
        warn!("vmsplice target fd {} is not a pipe", fd);
        return EBADF.as_isize();
    }
    if file.writable(){
        let nonblock = (flags & SPLICE_F_NONBLOCK) != 0 || (fd_status & (O_NONBLOCK | O_NDELAY)) != 0;
        let mut total_written = 0usize;

        for i in 0..iovcnt {
            let iov_addr = iov as usize + i * core::mem::size_of::<IoVec>();
            let iovec: IoVec = match try_translated_read(token, iov_addr as *const IoVec) {
                Some(io) => io,
                None => {
                    return if total_written == 0 {
                        EFAULT.as_isize()
                    } else {
                        total_written as isize
                    };
                }
            };
            if iovec.len == 0 {
                continue;
            }
            if iovec.len > IOV_BUF_MAX {
                return if total_written == 0 {
                    EINVAL.as_isize()
                } else {
                    total_written as isize
                };
            }

            if !prepare_user_write(token, iovec.base, iovec.len) {
                return if total_written == 0 {
                    EFAULT.as_isize()
                } else {
                    total_written as isize
                };
            }

            if nonblock && !file.ready_to_write() {
                return if total_written == 0 {
                    EAGAIN.as_isize()
                } else {
                    total_written as isize
                };
            }
            let user_buffer = UserBuffer::new(translated_byte_buffer(token, iovec.base as *const u8, iovec.len));
            if user_buffer.len() == 0 {
                return if total_written == 0 {
                    EFAULT.as_isize()
                } else {
                    total_written as isize
                };
            }

            // Always perform a one-shot nonblocking write to avoid self-deadlock
            // when producer and consumer progress happen in the same userspace loop.
            let written = match file.write_nonblock(user_buffer) {
                Ok(n) => n,
                Err(err) => {
                    if !nonblock && err.as_isize() == EAGAIN.as_isize() {
                        break;
                    }
                    return if total_written == 0 {
                        err.as_isize()
                    } else {
                        total_written as isize
                    };
                }
            };

            if written == 0 {
                if let Some(err) = file.check_write_error() {
                    return if total_written == 0 {
                        err.as_isize()
                    } else {
                        total_written as isize
                    };
                }
                break;
            }

            total_written += written;
            if written < iovec.len {
                break;
            }
        }
        total_written as isize
    }
    else{
        if let Some(iov) = try_translated_read(token, iov as *const IoVec) {
            let len = iov.len;
            file.read(UserBuffer::new(translated_byte_buffer(token, iov.base as *const u8, len))) as isize
        } else {
            EFAULT.as_isize()
        }
    }
}

pub fn sys_splice(fd_in: usize, off_in: *mut i64, fd_out: usize, off_out: *mut i64, len: usize, flags: u32) -> isize {
    warn!("fd_in , off_in , fd_out , off_out , len , flags : {} {} {} {} {} {}", fd_in, off_in as usize, fd_out, off_out as usize, len, flags);
    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;
    const SPLICE_FLAGS_MASK: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
    const SPLICE_CHUNK: usize = 64 * 1024;
    //无效的flag
    if (flags & !SPLICE_FLAGS_MASK) != 0 {
        return EINVAL.as_isize();
    }
    if len == 0 {
        return 0;
    }

    let task = match current_task() {
        Some(t) => t,
        None => return ENOSYS.as_isize(),
    };
    let token = current_user_token();
    drop(task);

    let files = current_files();
    let inner = files.exclusive_access();
    if fd_in >= inner.fds.len() || fd_out >= inner.fds.len() {
        return EBADF.as_isize();
    }
    //获取输入输出文件和状态
    let fd_in_entry = &inner.fds[fd_in];
    let file_in = match fd_in_entry.file.as_ref() {
        Some(f) => f.clone(),
        None => return EBADF.as_isize(),
    };
    let fd_in_status = fd_in_entry.status;

    let fd_out_entry = &inner.fds[fd_out];
    let file_out = match fd_out_entry.file.as_ref() {
        Some(f) => f.clone(),
        None => return EBADF.as_isize(),
    };
    let fd_out_status = fd_out_entry.status;
    drop(inner);

    if !file_in.readable() || !file_out.writable() {
        return EBADF.as_isize();
    }
    //判断是否有一方为管道
    let in_is_pipe = (file_in.get_stat().mode & S_IFMT) == crate::fs::S_IFIFO;
    let out_is_pipe = (file_out.get_stat().mode & S_IFMT) == crate::fs::S_IFIFO;

    // Linux splice requires at least one endpoint to be a pipe.
    if !in_is_pipe && !out_is_pipe {
        return EINVAL.as_isize();
    }
    // Pipe endpoints cannot use explicit offsets.
    if in_is_pipe && !off_in.is_null() {
        return EINVAL.as_isize();
    }
    if out_is_pipe && !off_out.is_null() {
        return EINVAL.as_isize();
    }
    //判断是否要非阻塞
    let nonblock = (flags & SPLICE_F_NONBLOCK) != 0
        || (fd_in_status & (O_NONBLOCK | O_NDELAY)) != 0
        || (fd_out_status & (O_NONBLOCK | O_NDELAY)) != 0;
    //提取偏移量
    let mut in_off = if off_in.is_null() {
        None
    } else {
        match try_translated_read(token, off_in as *const i64) {
            Some(v) if v >= 0 => Some(v as usize),
            Some(_) => return EINVAL.as_isize(),
            None => return EFAULT.as_isize(),
        }
    };
    let mut out_off = if off_out.is_null() {
        None
    } else {
        match try_translated_read(token, off_out as *const i64) {
            Some(v) if v >= 0 => Some(v as usize),
            Some(_) => return EINVAL.as_isize(),
            None => return EFAULT.as_isize(),
        }
    };
    //开始写入
    let mut total = 0usize;
    while total < len {
        //剩余未读入的长度，创造缓冲区
        let chunk = (len - total).min(SPLICE_CHUNK);
        let mut kbuf = vec![0u8; chunk];
        let read_slice = unsafe { core::slice::from_raw_parts_mut(kbuf.as_mut_ptr(), chunk) };
        let read_buf = UserBuffer { buffers: vec![read_slice] };
        //如果输入是非阻塞的且当前没有数据可读，立即返回
        if nonblock && !file_in.ready_to_read() {
            return if total == 0 {
                EAGAIN.as_isize()
            } else {
                total as isize
            };
        }

        let read_bytes = match in_off {
            Some(off) => {
                let n = file_in.read_at(off, read_buf);
                in_off = in_off.map(|v| v + n);
                n
            }
            None => file_in.read(read_buf),
        };

        if read_bytes == 0 {
            break;
        }

        let mut wrote = 0usize;
        while wrote < read_bytes {
            if nonblock && !file_out.ready_to_write() {
                return if total == 0 {
                    EAGAIN.as_isize()
                } else {
                    total as isize
                };
            }

            let write_slice = unsafe {
                core::slice::from_raw_parts_mut(kbuf.as_mut_ptr().add(wrote), read_bytes - wrote)
            };
            let write_buf = UserBuffer { buffers: vec![write_slice] };

            let n = match out_off {
                Some(off) => {
                    let n = file_out.write_at(off, write_buf);
                    out_off = out_off.map(|v| v + n);
                    n
                }
                None => file_out.write(write_buf),
            };

            if n == 0 {
                if let Some(err) = file_out.check_write_error() {
                    return if total == 0 {
                        err.as_isize()
                    } else {
                        total as isize
                    };
                }
                break;
            }

            wrote += n;
        }

        total += wrote;
        if wrote < read_bytes {
            break;
        }
    }

    if let Some(v) = in_off {
        if !try_translated_write(token, off_in, v as i64) {
            return if total == 0 {
                EFAULT.as_isize()
            } else {
                total as isize
            };
        }
    }
    if let Some(v) = out_off {
        if !try_translated_write(token, off_out, v as i64) {
            return if total == 0 {
                EFAULT.as_isize()
            } else {
                total as isize
            };
        }
    }

    //返回总共写入的字节数
    total as isize
}
pub fn sys_userfaultfd(_flags: i32) -> isize {
    // 需要 root 权限 (CAP_SYS_PTRACE)
    if current_euid() != 0 {
        return EPERM.as_isize();
    }

    let files = current_files();
    let mut inner = files.exclusive_access();
    let fd = match inner.alloc_fd(current_nofile_limit()) {
        Some(fd) => fd,
        None => return EMFILE.as_isize(),
    };
        inner.set_fd(fd, Arc::new(UserPageFaultInfo::new((_flags as usize & O_NONBLOCK) != 0)), FdFlags::from_bits_truncate(_flags as usize), 0);
        fd as isize
}
pub fn sys_symlinkat(target: *const u8, newdirfd: isize, linkpath: *const u8) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();

    let target_str = {
        if let Some(s) = try_translated_str(token, target) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    let linkpath_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, linkpath) {
            s
        } else {
            return EFAULT.as_isize();
        }
    );
    if linkpath_str.is_empty() || target_str.is_empty() {
        return ENOENT.as_isize();
    }
    debug!("kernel:pid[{}] sys_symlinkat: target={}, linkpath={}", task.getpid(), target_str, linkpath_str);

    // 解析起始目录
    let start_dentry = if linkpath_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if newdirfd == AT_FDCWD {
        current_pwd()
    } else {
        let files = current_files();
        let inner = files.exclusive_access();
        if newdirfd < 0 || newdirfd as usize >= inner.fds.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fds[newdirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return EBADF.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    // 检查目标是否已存在
    if start_dentry.find_tree(&linkpath_str, false).is_ok() {
        return EEXIST.as_isize();
    }

    // 解析父目录和文件名
    let parent_path_str = parent_path(&linkpath_str);
    let name = file_name(&linkpath_str);

    let parent_dentry = match start_dentry.find_tree(&parent_path_str, true) {
        Ok(d) => d,
        Err(_) => return ENOENT.as_isize(),
    };

    let _namespace_guard = parent_dentry.namespace_lock.lock();
    if parent_dentry.mounted_children.lock().contains_key(&name)
        || parent_dentry.children.lock().contains_key(&name)
    {
        return EEXIST.as_isize();
    }
    // 通过父目录的 inode 创建符号链接
    //warn!("parent_path_str={}, name={}, target_str={}", parent_dentry.inode.type_name(), name, target_str);
    if let Some(symlink_inode) = parent_dentry.inode.create_symlink(&name, &target_str) {
        // 将新创建的 Inode 挂到 VFS 树
        let mut children = parent_dentry.children.lock();
        let new_dentry = Dentry::new(
            name.clone(),
            symlink_inode,
            Arc::downgrade(&parent_dentry),
        );
        children.insert(name, new_dentry);
        0
    } else {
        EACCES.as_isize()
    }
}
/// fsync: 将文件描述符关联文件的数据同步到磁盘
/// 当前实现刷所有缓存而不是只刷指定文件
/// 
/// TODO: 完全实现 fsync 语义
pub fn sys_fsync(_fd: usize) -> isize {
    // crate::mm::mmap::sync_shared_page_cache();
    0
}

/// sync: 将所有文件系统缓存同步到磁盘
pub fn sys_sync() -> isize {
    crate::mm::mmap::sync_shared_page_cache();
    crate::drivers::block::cache::block_cache_sync_all();
    0
}
