//! 内存管理相关syscall，暂未完全迁移

use super::Errno::*;
use crate::{PAGE_SIZE, USER_APP_MAX_SIZE, mm::mmap::{self, MMapFlags}};
use super::*;

/// 内存映射
/// 
/// 参数说明：
/// 1. 长度不能为0，且地址必须页对齐
/// 2. 非匿名映射必须提供合法文件，且检查优先级高于长度
/// 3. 匿名映射不保证地址，且不允许提供文件
/// 4. 如果是非匿名，要求prot必须至少有PROT_READ
///
/// 
/// 参数检查由sys_mmap完成
pub fn sys_mmap(start: usize, len: usize, port: i32, flags: i32, fd: i32, off: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap called with start={:#x}, len={:#x}, prot={:#x}, flags={:#x}, fd={}, off={:#x}", 
        current_task().unwrap().process().pid.0, start, len, port, flags, fd, off);

    let mmap_flags = mmap::MMapFlags::from_bits_truncate(flags);
    let is_anonymous = mmap_flags.contains(mmap::MMapFlags::MAP_ANONYMOUS);
    let is_shared = mmap_flags.contains(mmap::MMapFlags::MAP_SHARED);

    // 要求flg不能为空
    if mmap_flags == MMapFlags::MAP_FILE {
        return Errno::EINVAL.as_isize();
    }

    let mmap_prot = mmap::MMapProt::from_bits_truncate(port);
    let read = mmap_prot.contains(mmap::MMapProt::PROT_READ);
    let write = mmap_prot.contains(mmap::MMapProt::PROT_WRITE);


    // 检查并提取文件对象
    let mut file_inner = if !is_anonymous {
        if fd < 0 {
            return Errno::EBADF.as_isize();
        }
        let task = current_task().unwrap();
        let process = task.process();
        let inner = process.inner_exclusive_access();
        let fd_usize = fd as usize;
        
        if fd_usize < inner.fd_table.len() {
            if let Some(file) = &inner.fd_table[fd_usize].file {
                Some(file.clone())
            } else {
                return Errno::EBADF.as_isize();
            }
        } else {
            return Errno::EBADF.as_isize();
        }
    } else {
        None
    };

    // 地址合法性检查
    if start + len >= USER_APP_MAX_SIZE || start % PAGE_SIZE != 0 {
        return Errno::EINVAL.as_isize();
    }

    // 长度不能为0
    if len == 0 {
        return Errno::EINVAL.as_isize();
    }

    // 文件权限检查
    if let Some(file) = &file_inner {
        if !file.readable() {
            return Errno::EACCES.as_isize();
        }
        if is_shared && write && !file.writable() {
            return Errno::EACCES.as_isize();
        }
    }
    
    //将 file_inner 和 off 逐层转发给 do_mmap
    let ret = match mmap::do_mmap(start, len, mmap_prot, mmap_flags, file_inner.clone(), off) {
        Ok(addr) => addr,
        Err(errno) => {
            return errno; // 直接返回错误码
        }
    };

    // 只有在非匿名且非共享才读取
    if !is_anonymous && !is_shared {
        if let Some(file) = file_inner {
            if file.readable() {
                let token = current_user_token();
                // 构造 UserBuffer，指向刚刚映射出来的用户态虚地址
                let user_buf = UserBuffer::new(translated_byte_buffer(token, ret as *const u8, len));
                // 使用 read_at 确保不受 FD 当前 offset 影响
                file.read_at(off, user_buf);
            }
        }
    }
    #[cfg(target_arch = "loongarch64")]
    // 手动刷新指令缓存
    unsafe { core::arch::asm!("ibar 0"); }
    
    debug!("[kernel] sys_mmap: mapped addr={:#x} for start={:#x}, len={:#x}, prot={:?}, flags={:?}", ret, start, len, mmap_prot, mmap_flags);
    ret as isize
}


pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    trace!("kernel:pid[{}] sys_munmap NOT COMPLITED", process.pid.0);
    if let Ok(_) = mmap::do_munmap(start,len) {
        0
    } else {
        EINVAL.as_isize() // 目标地址不合法
    }
}
