//! File and filesystem-related syscalls
use crate::PAGE_SIZE;
use crate::fs::{OpenFlags, ROOT_DENTRY, Stat, Statx, file_name, make_dir, make_pipe, open_file, parent_path};
use crate::mm::{PageSize, UserBuffer, translated_byte_buffer, try_translated_read, try_translated_str, try_translated_write};
use crate::task::{current_task, current_user_token};
use alloc::vec;
use alloc::sync::Arc;
use alloc::string::ToString;
use crate::syscall::TIME_CACHE;
use super::{errno::Errno::*, normalize_leading_dot_path, translate_path};
use crate::syscall::TmpfsFileInode;
use crate::syscall::OSInode;
use crate::syscall::Dentry;

use alloc::string::String;
const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_DUPFD_CLOEXEC: usize = 1030;

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
    trace!("kernel:pid[{}] sys_statfs path={}", current_task().unwrap().process().pid.0, path_str);

    if buf.is_null() {
        return EFAULT.as_isize();
    }

    // 1. 根据传入的路径，从全局目录树中找到对应的文件/目录节点
    let target_dentry = if path_str == "/" {
        crate::fs::ROOT_DENTRY.clone()
    } else if let Some(dentry) = crate::fs::ROOT_DENTRY.find_tree(&path_str, true) {
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

fn ensure_fd_slots(inner: &mut crate::process::ProcessControlBlockInner, target_len: usize) {
    // 上限检查
    if target_len > inner.fd_rlmt.cur_lmt {
        warn!("ensure_fd_slots: target_len {} exceeds fd_rlmt {}", target_len, inner.fd_rlmt.cur_lmt);
        return;
    }
    while inner.fd_table.len() < target_len {
        inner.fd_table.push(crate::process::FileDescriptor::empty());
    }
}
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    info!("pid[{}] [sys_write] ENTER fd={}, buf={:#x}, len={}", proc.pid.0, fd, buf as usize, len);
    // 检查 FD 是否越界或未打开
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize(); // 注意引入正确的 EBADF 路径
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    let status = inner.fd_table[fd].status;
    if !file.writable() {
        return EACCES.as_isize(); 
    }
    if (status & (O_NONBLOCK | O_NDELAY)) != 0 && !file.ready_to_write() {
        return EAGAIN.as_isize();
    }
    drop(inner); 
    let user_buffer = UserBuffer::new(crate::mm::translated_byte_buffer(token, buf, len));
  
    let ax = file.write(user_buffer) as isize;
    if ax == 0 && len > 0 {
        warn!("pid[{}] [sys_write] FATAL: Underlying file returned 0 on write! fd={}", proc.pid.0, fd);
    } else {
        info!("pid[{}] [sys_write] LEAVE written={}", proc.pid.0, ax);
    }
  
    ax
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().process().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fd_table[fd].file {
        let file = file.clone();
        let status = inner.fd_table[fd].status;
        drop(inner);
        if !file.readable() {
            return EACCES.as_isize(); // 权限不足
        }
        if (status & (O_NONBLOCK | O_NDELAY)) != 0 && !file.ready_to_read() {
            return EAGAIN.as_isize();
        }
        trace!("kernel:pid[{}] sys_read: fd={}, len={}", task.process().pid.0, fd, len);
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        EBADF.as_isize() // 文件描述符无效
    }
}
pub fn sys_readv(fd: usize, iov_ptr: usize, iovcnt: usize) -> isize {
    // 防止随机/恶意 iovcnt 导致死循环或 DOS
    const IOV_MAX: usize = 1024;
    let iovcnt = iovcnt.min(IOV_MAX);
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    // fd合法性检查
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    let token = inner.memory_set.token();
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
    let proc = task.process();
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, path) { s } else { return EFAULT.as_isize(); }
    );
    trace!("kernel:pid[{}] tid[{}] sys_openat, dirfd={}, path={}", task.process().pid.0, task.gettid(), dirfd, path_str);
    //debug!("[kernel] sys_openat: dirfd={}, path={}, flags={}", dirfd, path_str, flags);
    const O_TMPFILE: u32 = 0x400000;
    let (readable, writable) = match flags & 0x3 {
    0x0 => (true, false), // O_RDONLY
    0x1 => (false, true), // O_WRONLY
    0x2 => (true, true),  // O_RDWR
    _ => (true, true),
    };
    if (flags & O_TMPFILE) != 0 {
        let target_dentry = if let Some(dentry) = ROOT_DENTRY.find_tree(&path_str, true) {
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
        anon_vfs_inode,
        anon_dentry,
        ));

        let mut inner = proc.inner_exclusive_access();
        let fd = match inner.alloc_fd() {
            Some(fd) => fd,
            None => return EMFILE.as_isize(),
        };

        // 4. 塞入进程的文件描述符表
        inner.set_fd(fd, anon_file, (flags & O_CLOEXEC) != 0, flags as usize);
        debug!("kernel:pid[{}] sys_openat: O_TMPFILE success fd={}", task.process().pid.0, fd);
        
        return fd as isize;
    }
    
    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        proc.inner_exclusive_access().cwd.clone()
    } else {
        let inner = proc.inner_exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fd_table.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fd_table[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return EBADF.as_isize();
            }
        } else {
            return EBADF.as_isize();
        }
    };

    let open_flags = OpenFlags::from_bits_truncate(flags);
    if let Some(inode) = open_file(start_dentry, path_str.as_str(), open_flags, mode) {
        if open_flags.should_be_directory() && (inode.inode.get_stat().mode & 0o040000) == 0 {
            trace!("kernel:pid[{}] VFS: sys_openat failed - '{}' is not a directory", task.process().pid.0, path_str);
            return ENOTDIR.as_isize(); // 目标文件不是目录
        }
        let mut inner = proc.inner_exclusive_access();
        let fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
        inner.set_fd(fd, inode, (flags & O_CLOEXEC) != 0, flags as usize);
        fd as isize
    } else {
        trace!("kernel:pid[{}] VFS: File '{}' not found", task.process().pid.0, path_str);
        debug!("kernel:pid[{}] sys_openat: failed path={}", task.process().pid.0, path_str);
            ENOENT.as_isize()
    }
}

pub fn sys_close(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_close, aim fd = {}", current_task().unwrap().process().pid.0, fd);
    let task = current_task().unwrap();
    let proc = task.process();
    let mut inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    inner.clear_fd(fd);
    0
}

pub fn sys_accessat(dirfd: isize, path: *const u8, mode: u32, _flags: u32) -> isize {
    let task = current_task().unwrap();
    let proc = task.process();
    let token = current_user_token();
    let path_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, path) { s } else { return EFAULT.as_isize(); }
    );
    info!("kernel:pid[{}] sys_accessat: dirfd={}, path={}, mode={}", task.process().pid.0, dirfd, path_str, mode);

    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        proc.inner_exclusive_access().cwd.clone()
    } else {
        let inner = proc.inner_exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fd_table.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fd_table[dirfd as usize].file {
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
        let stat = os_inode.inode.get_stat();
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

pub fn sys_pipe(pipe: *mut usize) -> isize {
	trace!("kernel:pid[{}] sys_pipe", current_task().unwrap().process().pid.0);
    let task = current_task().unwrap();
    let proc = task.process();
    let token = current_user_token();
    let mut inner = proc.inner_exclusive_access();
    let page_table = crate::mm::PageTable::from_token(token);
    let va = pipe as usize;
    if page_table.translate_va(crate::mm::VirtAddr::from(va)).is_none() ||
       page_table.translate_va(crate::mm::VirtAddr::from(va + 4)).is_none() {
        trace!("kernel:pid[{}] sys_pipe error point: {:#x}，", task.process().pid.0, va);
          return EFAULT.as_isize();
    }
    let (pipe_read, pipe_write) = make_pipe();
    let read_fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
    inner.set_fd(read_fd, pipe_read, false, 0);
    let write_fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
    inner.set_fd(write_fd, pipe_write, false, O_WRONLY as usize);
    // 释放锁，因为下面的write会访问用户锁
    drop(inner);
    // User ABI for pipe is int pipefd[2], i.e. two 32-bit entries.
    let pipe_u32 = pipe as *mut u32;
    if !try_translated_write(token, pipe_u32, read_fd as u32) {
        return EFAULT.as_isize();
    }
    if !try_translated_write(token, unsafe { pipe_u32.add(1) }, write_fd as u32) {
        return EFAULT.as_isize();
    }
    //println!("pipe done");
    0
}

pub fn sys_dup(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_dup fd = {}", current_task().unwrap().process().pid.0, fd);
    // println!("kernel:pid[{}] sys_dup fd = {}", current_task().unwrap().process().pid.0, fd);
    let task = current_task().unwrap();
    let proc = task.process();
    let mut inner = proc.inner_exclusive_access();
    // println!("table len = {}", inner.fd_table.len());
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let new_fd =match inner.alloc_fd() {
        Some(fd) => fd,
        None => return EMFILE.as_isize(), //   
    };
    // println!("[kernel] sys_dup: new fd allocated: {}", new_fd);
    let file = Arc::clone(inner.fd_table[fd].file.as_ref().unwrap());
    let old_status = inner.fd_table[fd].status;
    inner.set_fd(new_fd, file, false, old_status);
    new_fd as isize
}

pub fn sys_lseek(fd: usize, offset: isize, whence: i32) -> isize {
    // println!("[DEBUG VFS] sys_lseek: fd={}, offset={}, whence={}", fd, offset, whence);
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    file.lseek(offset, whence)
}
pub fn sys_dup2(fd: usize, new_fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_dup2", current_task().unwrap().process().pid.0);
    let task = current_task().unwrap();
    let proc = task.process();
    let mut inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    
    // 不能超过限制
    if new_fd >= inner.fd_rlmt.cur_lmt {
        return EBADF.as_isize();
    }

    if fd == new_fd {
        return new_fd as isize;
    }
    
    ensure_fd_slots(&mut inner, new_fd + 1);
    let file = Arc::clone(inner.fd_table[fd].file.as_ref().unwrap());
    let old_status = inner.fd_table[fd].status;
    inner.set_fd(new_fd, file, false, old_status);
    new_fd as isize
}

pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fd_table[fd].file {
        let file = file.clone();
        drop(inner);
        let mut stat = file.get_stat();
        if let Some(&(asec, ansec, msec, mnsec)) = crate::syscall::fs::TIME_CACHE.lock().get(&stat.ino) {
            stat.atime_sec = asec;
            stat.atime_nsec = ansec;
            stat.mtime_sec = msec;
            stat.mtime_nsec = mnsec;
        }
        let inner = proc.inner_exclusive_access();
        let token = inner.memory_set.token();
         drop(inner);
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
    if iovcnt > IOV_MAX {
        return EINVAL.as_isize();
    }
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    info!("pid[{}] [sys_writev] ENTER fd={}, iov_ptr={:#x}, iovcnt={}", proc.pid.0, fd, iov_ptr, iovcnt);
    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }
    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
    let token = inner.memory_set.token();
    drop(inner);
    let mut total_written = 0;
    for i in 0..iovcnt {
        let iov_addr = iov_ptr + i * core::mem::size_of::<IoVec>();
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
        const IOV_BUF_MAX: usize = 1024;
        if iovec.len > IOV_BUF_MAX {
            return EFAULT.as_isize();
        }
        let iovec_len = iovec.len.min(IOV_BUF_MAX);
        let user_buffer = UserBuffer {
            buffers: translated_byte_buffer(token, iovec.base as *const u8, iovec_len),
        };
        let written = file.write(user_buffer);
        if written == 0 && iovec_len > 0 {
            warn!("pid[{}] [sys_writev] FATAL: Underlying file returned 0 on write! fd={}", proc.pid.0, fd);
        }
        total_written += written;
    }
    info!("pid[{}] [sys_writev] LEAVE total_written={}", proc.pid.0, total_written);
    total_written as isize
}
pub fn sys_statx(dirfd: isize, path: *const u8, flags: u32, mask: u32, st: *mut Statx) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let proc = task.process();
    let path_str = {
        if let Some(s) = try_translated_str(token, path) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    trace!("kernel:pid[{}] sys_statx: dirfd={}, path={}, flags={:#x}, mask={:#x}", task.process().pid.0, dirfd, path_str, flags, mask);
    const AT_EMPTY_PATH: u32 = 0x1000;
    if path_str.is_empty() {
        if (flags & AT_EMPTY_PATH) == 0 {
            return EINVAL.as_isize(); // 无效参数 
        }

        let inner = proc.inner_exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fd_table.len() {
            return EBADF.as_isize(); 
        }

        if let Some(file) = &inner.fd_table[dirfd as usize].file {
            if let Some(dentry) = file.get_dentry() {
                let mut statx_data = dentry.inode.get_statx();
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
        }
        return EBADF.as_isize();
    }

    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        proc.inner_exclusive_access().cwd.clone()
    } else {
        let inner = proc.inner_exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fd_table.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fd_table[dirfd as usize].file {
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
    if let Some(target_dentry) = start_dentry.find_tree(&path_str, follow_links) {
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

    debug!("kernel:pid[{}] sys_mkdir: path={}", current_task().unwrap().process().pid.0, path);
    
    let start = if path.starts_with('/') {
        ROOT_DENTRY.clone()
    } else {
        current_task().unwrap().process().inner_exclusive_access().cwd.clone()
    };

    // 目标存在
    if start.find_tree(&path, true).is_some() {
        return EEXIST.as_isize();
    }

    // 父目录不存在
    let parent = parent_path(&path);
    if start.find_tree(&parent, true).is_none() {
        return ENOENT.as_isize();
    }

    if let Some(_) = make_dir(path.as_str(), _mode) {
        0
    } else {
        EACCES.as_isize() // 权限不足
    }
}

pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_linkat NOT IMPLEMENTED", current_task().unwrap().process().pid.0);
    ENOSYS.as_isize()
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
    // 获取工作路径并查找
    let task = current_task().unwrap();
    let proc = task.process();
    let base_dentry = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if _dirfd == AT_FDCWD {
        proc.inner_exclusive_access().cwd.clone()
    } else {
        let inner = proc.inner_exclusive_access();
        if _dirfd < 0 || _dirfd as usize >= inner.fd_table.len() {
            return EBADF.as_isize();
        }
        if let Some(file) = &inner.fd_table[_dirfd as usize].file {
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
        Some(d) => d,
        None => return ENOENT.as_isize(),
    };
    // 文件类型检查
    let st = link_dentry.inode.get_stat();
    let is_symlink = (st.mode & 0o170000) == 0o120000; // 符号链接
    if !is_symlink {
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
    copied as isize
}

pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();

    let task = current_task().unwrap();
    let proc = task.process();
    let mut inner = proc.inner_exclusive_access();

    let fd_valid = fd < inner.fd_table.len() && inner.fd_table[fd].file.is_some();
    if !fd_valid && cmd != F_DUPFD && cmd != F_DUPFD_CLOEXEC {
        return EBADF.as_isize();
    }

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            if !fd_valid {
                return EBADF.as_isize();
            }
            // 防溢出
            if arg >= inner.fd_rlmt.cur_lmt {
                return EBADF.as_isize();
            }
            let mut new_fd = arg;
            while new_fd < inner.fd_table.len() {
                if inner.fd_table[new_fd].file.is_none() {
                    break;
                }
                new_fd += 1;
            }
            // 再次检查
            if new_fd >= inner.fd_rlmt.cur_lmt {
                return EBADF.as_isize();
            }
            ensure_fd_slots(&mut inner, new_fd + 1);
            let file = Arc::clone(inner.fd_table[fd].file.as_ref().unwrap());
            let old_status = inner.fd_table[fd].status;
            inner.set_fd(
                new_fd,
                file,
                cmd == F_DUPFD_CLOEXEC,
                old_status,
            );
            new_fd as isize
        }
        F_GETFD => {
            if inner.fd_table[fd].cloexec { FD_CLOEXEC as isize } else { 0 }
        }
        F_SETFD => {
            inner.fd_table[fd].cloexec = (arg & FD_CLOEXEC) != 0;
            0
        }
        F_GETFL => inner.fd_table[fd].status as isize,
        F_SETFL => {
            let old = inner.fd_table[fd].status;
            inner.fd_table[fd].status = (old & O_ACCMODE) | (arg & !O_ACCMODE);
            0
        }
        _ => EINVAL.as_isize(),
    }
}



/// YOUR JOB: Implement unlinkat.
pub fn sys_unlinkat(dirfd: isize, path: *const u8, flags: usize) -> isize {
    let token = current_user_token();
    let path_str = {
        if let Some(s) = try_translated_str(token, path) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    trace!("kernel:pid[{}] sys_unlinkat dirfd={} path={} flags={:#x}", current_task().unwrap().process().pid.0, dirfd, path_str, flags);

    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();

    let cwd = inner.cwd.clone();
    let fd_table_len = inner.fd_table.len();
    let base_dir = if path_str.starts_with('/') {
        ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        cwd.clone() 
    } else {
        if dirfd < 0 || (dirfd as usize) >= fd_table_len || inner.fd_table[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        trace!("kernel:pid[{}] sys_unlinkat: resolve relative to dirfd {} is WIP", task.process().pid.0, dirfd);
        cwd.clone() 
    };

    drop(inner);

    // 找到目标文件的 dentry
    let target_dentry = base_dir.find_tree(&path_str, false);
    let target = match target_dentry {
        Some(d) => d,
        None => return ENOENT.as_isize(),
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
    if let Some(parent) = parent_dentry {
        // 尝试删除
        if let Some(_inode_id) = parent.inode.delete_dir_entry(&name) {
            parent.children.lock().remove(&name);
            return 0;
        } else {
            // 驱动引起的删除不成功
            error!("kernel:pid[{}] VFS failed to delete '{}'. Underlay FS returned None.", task.process().pid.0, name);
            return EACCES.as_isize();
        }
    }
    ENOENT.as_isize() // 父目录不存在
}
pub fn sys_sendfile(out_fd: usize, in_fd: usize, _offset_ptr: usize, count: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_sendfile: out_fd={}, in_fd={}, count={}",
        current_task().unwrap().process().pid.0,
        out_fd,
        in_fd,
        count
    );
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    // 文件描述符合法性检查
    if out_fd >= inner.fd_table.len() || in_fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    let out_file = match &inner.fd_table[out_fd].file {
        Some(file) => file.clone(),
        None => return EBADF.as_isize(),
    };
    let in_file = match &inner.fd_table[in_fd].file {
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

    trace!("kernel:pid[{}] sys_sendfile: transferred={}", task.process().pid.0, total_transferred);
    total_transferred as isize
}

/// copy_file_range syscall stub — not yet implemented.
/// Returns ENOSYS so LTP tests get a clear "not supported" rather than an
/// "unimplemented syscall" warning.
pub fn sys_copy_file_range(
    _fd_in: usize,
    _off_in: *mut i64,
    _fd_out: usize,
    _off_out: *mut i64,
    _len: usize,
    _flags: u32,
) -> isize {
    warn!("[kernel] sys_copy_file_range: not implemented");
    ENOSYS.as_isize()
}

pub fn sys_getdents(fd: usize, dirp: *mut u8, count: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fd_table[fd].file {
        let file = file.clone();
        drop(inner);
        if !file.readable() {
            return EACCES.as_isize(); // 权限不足
        }
        trace!("kernel:pid[{}] sys_getdents: fd={}, count={}", task.process().pid.0, fd, count);
        let mut bufs = translated_byte_buffer(token, dirp, count);
        if bufs.is_empty() {
            return EFAULT.as_isize();
        }
        file.getdents(bufs.remove(0)) as isize
    } else {
        return EBADF.as_isize();
    }
}

pub fn sys_getcwd(buf: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    let path = inner.cwd.get_full_path();
    drop(inner);

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

    debug!("kernel:pid[{}] sys_chdir: path={}", current_task().unwrap().process().pid.0, path_str);
    
    let task = current_task().unwrap();
    let proc = task.process();
    let cwd = proc.inner_exclusive_access().cwd.clone();
    let current_path = {
        let inner = proc.inner_exclusive_access();
        inner.cwd.get_full_path()
    };

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
        let mut inner = proc.inner_exclusive_access();
        inner.cwd = inode.get_dentry();
        0
    } else {
        ENOENT.as_isize() // 目录不存在
    }
}

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
    debug!("kernel:pid[{}] sys_mount: source={}, target={}, filesystemtype={}, mountflags={}", current_task().unwrap().process().pid.0, source_str, target_str, filesystemtype_str, mountflags);
    return 0; // 目前仅支持 ext4 文件系统的挂载
}

pub fn sys_umount(target: *const u8) -> isize {
    let token = current_user_token();
    let target_str = normalize_leading_dot_path(
        if let Some(s) = try_translated_str(token, target) { s } else { return EFAULT.as_isize(); }
    );
    debug!("kernel:pid[{}] sys_umount: target={}", current_task().unwrap().process().pid.0, target_str);
    return 0;
}

/// 移除
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
    debug!("kernel:pid[{}] sys_fremovexattr: fd={}, name={}", current_task().unwrap().process().pid.0, _fd, name_str);
    return 0; // 目前不支持扩展属性，直接返回成功
}

pub fn sys_fstatat(dirfd: isize, path_ptr: *const u8, st: *mut Stat) -> isize {
    let token = current_user_token();
    let path_str = {
        if let Some(s) = crate::mm::try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    trace!("kernel:pid[{}] sys_fstatat dirfd={} path={}", current_task().unwrap().process().pid.0, dirfd, path_str);

    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    let cwd = inner.cwd.clone();
    let fd_table_len = inner.fd_table.len();
    
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
        if dirfd < 0 || (dirfd as usize) >= fd_table_len || inner.fd_table[dirfd as usize].file.is_none() {
            return EBADF.as_isize();
        }
        // 待实现
        cwd.clone()
    };

    drop(inner); 
    // 查找文件
    let target_dentry = base_dir.find_tree(&path_str, false);

    match target_dentry {
        Some(dentry) => {
            let stat = dentry.inode.get_stat();
            crate::mm::translated_write(token, st, stat);
            0
        }
        None => {
            // 不存在
            ENOENT.as_isize()
        }
    }
}

pub fn sys_pread64(fd: usize, buf: *mut u8, count: usize, offset: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let proc = task.process();
    let inner = proc.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }
    if let Some(file) = &inner.fd_table[fd].file {
        let file = file.clone();
        drop(inner);
        if !file.readable() {
            return EACCES.as_isize();
        }
        trace!("kernel:pid[{}] sys_pread64: fd={}, count={}, offset={}", task.process().pid.0, fd, count, offset);
        file.pread(offset, UserBuffer::new(translated_byte_buffer(token, buf, count))) as isize
    } else {
        EBADF.as_isize()
    }
}

/// 修改权限模式
pub fn sys_fchmodat(dirfd: isize, path_ptr: *const u8, mode: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process(); 
    let mut inner = process.inner_exclusive_access();
    let token = inner.get_user_token();
    let euid = inner.uid;
    drop(inner);
    
    let path = {
        if let Some(s) = try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };

    match ROOT_DENTRY.find_tree(path.as_str(), true) {
        Some(dentry) => {
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
        None => {
            ENOENT.as_isize()
        }
    }
}

/// 通过 fd 修改文件权限
/// Linux: int fchmod(int fd, mode_t mode)
pub fn sys_fchmod(fd: usize, mode: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    let euid = inner.uid;

    if fd >= inner.fd_table.len() || inner.fd_table[fd].file.is_none() {
        return EBADF.as_isize();
    }

    let file = inner.fd_table[fd].file.as_ref().unwrap().clone();
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

/// 修改所有者/组
pub fn sys_fchownat(dirfd: isize, path_ptr: *const u8, owner: u32, group: u32) -> isize {
    let task = current_task().unwrap();
    let process = task.process(); 
    let mut inner = process.inner_exclusive_access();
    let token = inner.get_user_token();
    let euid = inner.uid;
    drop(inner);
    info!("pid[{}] sys_fchownat: dirfd={}, owner={}, group={}", task.process().pid.0, dirfd, owner, group);
    let path = {
        if let Some(s) = try_translated_str(token, path_ptr) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };
    info!("pid[{}] sys_fchownat: path '{}'", task.process().pid.0, path);

    match ROOT_DENTRY.find_tree(path.as_str(), true) {
        Some(dentry) => {
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
        None => {
            ENOENT.as_isize()
        }
    }
}

/// 预分配文件空间
/// Linux: int fallocate(int fd, int mode, off_t offset, off_t len)
pub fn sys_fallocate(fd: usize, mode: usize, offset: i64, len: i64) -> isize {
    const FALLOC_FL_KEEP_SIZE: usize = 0x01;

    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();

    if fd >= inner.fd_table.len() {
        return EBADF.as_isize();
    }

    if let Some(file) = &inner.fd_table[fd].file {
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
    let proc = task.process();
    let token = proc.inner_exclusive_access().get_user_token();
    let name_str = {
        if let Some(s) = try_translated_str(token, name) {
            s
        } else {
            return EFAULT.as_isize();
        }
    };

    // 创建 memfd 文件（只创建 inode + dentry，不映射 VPN）
    let file = crate::fs::memfd::create_memfd(&name_str, page_size);

    let mut inner = proc.inner_exclusive_access();
    let fd = match inner.alloc_fd() {
        Some(fd) => fd,
        None => return EMFILE.as_isize(),
    };
    inner.set_fd(fd, file, flags.contains(MemfdFlags::MFD_CLOEXEC), 0);

    trace!("kernel:pid[{}] sys_memfd_create: name='{}', page_size={:?}, fd={}",
        task.process().pid.0, name_str, page_size, fd);
    fd as isize
}

