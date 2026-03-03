//! File and filesystem-related syscalls
use crate::fs::{make_pipe, OpenFlags, Stat, open_file, make_dir, parent_path, file_name};
use crate::mm::{translated_byte_buffer, translated_refmut, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};
use alloc::sync::Arc;

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();

    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("[kernel] sys_write: fd={}, len={}", fd, len);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("[kernel] sys_read: fd={}, len={}", fd, len);
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

const AT_FDCWD: isize = -100;

pub fn sys_openat(dirfd: isize, path: *const u8, flags: u32, _mode: u32) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let path_str = translated_str(token, path);
    debug!("[kernel] sys_openat: dirfd={}, path={}, flags={}", dirfd, path_str, flags);

    let start_dentry = if path_str.starts_with('/') {
        crate::fs::ROOT_DENTRY.clone()
    } else if dirfd == AT_FDCWD {
        task.inner_exclusive_access().cwd.clone()
    } else {
        let inner = task.inner_exclusive_access();
        if dirfd < 0 || dirfd as usize >= inner.fd_table.len() {
            return -1;
        }
        if let Some(file) = &inner.fd_table[dirfd as usize] {
            if let Some(dentry) = file.get_dentry() {
                dentry
            } else {
                return -1;
            }
        } else {
            return -1;
        }
    };

    if let Some(inode) = open_file(start_dentry, path_str.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        if OpenFlags::from_bits(flags).unwrap().should_be_directory() && (inode.inode.get_stat().mode & 0o040000) == 0 {
            trace!("VFS: sys_openat failed - '{}' is not a directory", path_str);
            return -1;
        }
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

pub fn sys_close(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}

pub fn sys_pipe(pipe: *mut usize) -> isize {
	trace!("kernel:pid[{}] sys_pipe", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let mut inner = task.inner_exclusive_access();
    let (pipe_read, pipe_write) = make_pipe();
    let read_fd = inner.alloc_fd();
    inner.fd_table[read_fd] = Some(pipe_read);
    let write_fd = inner.alloc_fd();
    inner.fd_table[write_fd] = Some(pipe_write);
    *translated_refmut(token, pipe) = read_fd;
    *translated_refmut(token, unsafe { pipe.add(1) }) = write_fd;
    0
}

pub fn sys_dup(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_dup", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    let new_fd = inner.alloc_fd();
    inner.fd_table[new_fd] = Some(Arc::clone(inner.fd_table[fd].as_ref().unwrap()));
    new_fd as isize
}

pub fn sys_dup2(fd: usize, new_fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_dup2", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() || inner.fd_table[fd].is_none() {
        return -1;
    }
    if fd == new_fd {
        return new_fd as isize;
    }
    while new_fd >= inner.fd_table.len() {
        inner.fd_table.push(None);
    }
    inner.fd_table[new_fd] = Some(Arc::clone(inner.fd_table[fd].as_ref().unwrap()));
    new_fd as isize
}

pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        drop(inner);
        let stat = file.get_stat();
        *translated_refmut(token, st) = stat;
        0
    } else {
        -1
    }
}

pub fn sys_statx(_dirfd: isize, _pathname: *const u8, _mask: u32, _flags: u32, _st: *mut Stat) -> isize {
    //todo
    sys_fstat(_dirfd as usize, _st)
}

pub fn sys_mkdir(path: *const u8, _mode: u32) -> isize {
    let token = current_user_token();
    let path = translated_str(token, path);
    debug!("[kernel] sys_mkdir: path={}", path);
    
    if let Some(_) = make_dir(path.as_str(), _mode) {
        0
    } else {
        -1
    }
}
pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_linkat NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

pub fn sys_unlinkat(path: *const u8) -> isize {
    let token = current_user_token();
    let path_str = translated_str(token, path);
    trace!("kernel:pid[{}] sys_unlinkat path={}", current_task().unwrap().pid.0, path_str);
    
    let parent_path_str = parent_path(&path_str);
    let name = file_name(&path_str);
    
    let task = current_task().unwrap();
    let cwd = task.inner_exclusive_access().cwd.clone();
    
    if let Some(parent_dentry) = cwd.find_tree(&parent_path_str) {
        if let Some(_inode_id) = parent_dentry.inode.delete_dir_entry(&name) {
            // 清理 Dentry 缓存，确保下次也读不到
            parent_dentry.children.lock().remove(&name);
            return 0;
        }
    }
    -1
}

pub fn sys_getdents(fd: usize, dirp: *mut u8, count: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        drop(inner);
        if !file.readable() {
            return -1;
        }
        trace!("[kernel] sys_getdents: fd={}, count={}", fd, count);
        file.getdents(translated_byte_buffer(token, dirp, count).remove(0)) as isize
    } else {
        -1
    }
}

pub fn sys_getcwd(buf: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    let path = inner.cwd.get_full_path();
    drop(inner);

    let path_bytes = path.as_bytes();
    if path_bytes.len() + 1 > size {
        return -1;
    }
    let mut user_buf = UserBuffer::new(translated_byte_buffer(token, buf, size));
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
    let path_str = translated_str(token, path);
    debug!("[kernel] sys_chdir: path={}", path_str);
    
    let task = current_task().unwrap();
    let cwd = task.inner_exclusive_access().cwd.clone();
    let current_path = {
        let inner = task.inner_exclusive_access();
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

    if let Some(inode) = open_file(cwd, full_path.as_str(), OpenFlags::DIRECTORY) {
        let mut inner = task.inner_exclusive_access();
        inner.cwd = inode.get_dentry();
        0
    } else {
        -1
    }
}

pub fn sys_mount(source: *const u8, target: *const u8, filesystemtype: *const u8, mountflags: u32) -> isize {
    let token = current_user_token();
    let source_str = translated_str(token, source);
    let target_str = translated_str(token, target);
    let filesystemtype_str = translated_str(token, filesystemtype);
    debug!("[kernel] sys_mount: source={}, target={}, filesystemtype={}, mountflags={}", source_str, target_str, filesystemtype_str, mountflags);
    return 0;    // 目前仅支持 ext4 文件系统的挂载
}

pub fn sys_umount(target: *const u8) -> isize {
    let token = current_user_token();
    let target_str = translated_str(token, target);
    debug!("[kernel] sys_umount: target={}", target_str);
    return 0;
}
