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
/// 5
/// 
/// 参数检查由sys_mmap完成
pub fn sys_mmap(start: usize, len: usize, port: i32, flags: i32, fd: i32, off: usize) -> isize {
    info!("kernel:pid[{}] sys_mmap called with start={:#x}, len={:#x}, prot={:#x}, flags={:#x}, fd={}, off={:#x}", 
        current_task().unwrap().process().pid.0, start, len, port, flags, fd, off);

    // MAP_SHARED_VALIDATE (0x03): 等同于 MAP_SHARED 但需要校验所有 flag 位已知
    // 必须在 from_bits_truncate 之前检查，因为 truncate 会丢弃未知位
    if (flags & mmap::MAP_SHARED_VALIDATE) == mmap::MAP_SHARED_VALIDATE {
        let all_known = mmap::MMapFlags::all().bits();
        if (flags & !all_known) != 0 {
            return Errno::EOPNOTSUPP.as_isize();
        }
    }

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

    // 文件偏移需要页对齐
    if off % PAGE_SIZE != 0 {
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

    // 处理 MAP_LOCKED：记录锁定的内存量（用于 /proc/self/status VmLck）
    // 目前是伪实现，只单纯记录，实际上没“阻止换出”
    // 但是当前内核没有真正的swap，所以也不需要阻止换出（所有页都在内存中）
    if mmap_flags.contains(mmap::MMapFlags::MAP_LOCKED) {
        let task = current_task().unwrap();
        let proc = task.process();
        let mut inner = proc.inner_exclusive_access();
        inner.locked_bytes = inner.locked_bytes.saturating_add(len);
    }

    ret as isize
}


pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    
    // 参数检查：地址必须页对齐，长度不能为0，且映射区域不能超过用户空间上限
    if start + len >= USER_APP_MAX_SIZE || start % PAGE_SIZE != 0 || len == 0 {
        return EINVAL.as_isize();
    }

    trace!("kernel:pid[{}] sys_munmap NOT COMPLITED", process.pid.0);
    if let Ok(_) = mmap::do_munmap(start,len) {
        0
    } else {
        EINVAL.as_isize()
    }
}

// 回写内存映射区域到文件
// 目前的实现回写的包括非共享页缓存
pub fn sys_msync(_addr: usize, _len: usize, _flags: u32) -> isize {
    crate::mm::mmap::sync_shared_page_cache();
    0
}

/// madvise - 给内核关于内存使用的建议
///
/// 参数:
/// - addr: 起始地址（必须页对齐）
/// - len: 长度
/// - advice: 建议类型
///
/// 返回值: 成功返回 0
pub fn sys_madvise(addr: usize, len: usize, advice: i32) -> isize {
    const MADV_NORMAL: i32 = 0;
    const MADV_RANDOM: i32 = 1;
    const MADV_SEQUENTIAL: i32 = 2;
    const MADV_WILLNEED: i32 = 3;
    const MADV_DONTNEED: i32 = 4;
    const MADV_FREE: i32 = 8;

    // 对齐到页边界
    if addr % PAGE_SIZE != 0
        || len % PAGE_SIZE != 0 
        || addr + len >= USER_APP_MAX_SIZE
    {
        return EINVAL.as_isize();
    }


    match advice {
        MADV_DONTNEED => {
            // MADV_DONTNEED: 告知内核这些页不再需要，可以释放
            // 对于匿名映射，等同于 munmap；内核会释放物理页
            match mmap::do_munmap(addr, len) {
                Ok(_) => 0,
                Err(_) => 0, // 不必报错，静默忽略
            }
        }
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_FREE => {
            // 使用建议（优化用），伪实现
            0
        }
        _ => {
            EINVAL.as_isize()
        }
    }
}