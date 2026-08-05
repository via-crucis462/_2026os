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
    info!("kernel:pid[{}] sys_mmap called with start={:#x}, len={:#x}, prot={:#x}, flags={:#x}, fd={}, off={:#x}", 
        current_task().unwrap().getpid(), start, len, port, flags, fd, off);

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
    let file_inner = if !is_anonymous {
        if fd < 0 {
            return Errno::EBADF.as_isize();
        }
        let task = current_task().unwrap();
        let files = task.inner_exclusive_access().files.clone();
        let files = files.exclusive_access();
        let fd_usize = fd as usize;
        
        if fd_usize < files.fds.len() {
            if let Some(file) = &files.fds[fd_usize].file {
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
        Err(errno) => return errno,
    };
    
    debug!("[kernel] sys_mmap: mapped addr=0x{:x} for start=0x{:x}, len=0x{:x}, prot={:?}, flags={:?}", ret, start, len, mmap_prot, mmap_flags);

    // 处理 MAP_LOCKED：记录锁定的内存量（用于 /proc/self/status VmLck）
    // 目前是伪实现，只单纯记录，实际上没“阻止换出”
    // 但是当前内核没有真正的swap，所以也不需要阻止换出（所有页都在内存中）
    if mmap_flags.contains(mmap::MMapFlags::MAP_LOCKED) {
        let task = current_task().unwrap();
        let mut inner = task.inner_exclusive_access();
        inner.locked_bytes = inner.locked_bytes.saturating_add(len);
    }

    ret as isize
}


pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    // let task = current_task().unwrap();
    // let pid = task.getpid();
    
    // 参数检查：地址必须页对齐，长度不能为0，且映射区域不能超过用户空间上限
    if start + len >= USER_APP_MAX_SIZE || start % PAGE_SIZE != 0 || len == 0 {
        return EINVAL.as_isize();
    }

    trace!("kernel:pid[{}] sys_munmap NOT COMPLITED", task.getpid());
    if let Ok(_) = mmap::do_munmap(start,len) {
        0
    } else {
        EINVAL.as_isize()
    }
}

const MREMAP_MAYMOVE: usize = 1;
const MREMAP_FIXED: usize = 2;

/// mremap - 扩大/缩小/移动内存映射
///
/// 目前实现：
/// - 缩小：直接 munmap 尾部；
/// - 扩大且尾部空闲：原地扩展（匿名映射）；
/// - 否则若带 MREMAP_MAYMOVE：新映射 + 拷贝 + 解除旧映射；
/// - MREMAP_FIXED 暂未实现，返回 EINVAL。
pub fn sys_mremap(
    old_addr: usize,
    old_size: usize,
    new_size: usize,
    flags: usize,
    new_addr: usize,
) -> isize {
    let page = PAGE_SIZE;
    if old_addr % page != 0
        || old_size == 0
        || new_size == 0
        || old_addr.checked_add(old_size).map_or(true, |e| e >= USER_APP_MAX_SIZE)
        || old_addr.checked_add(new_size).map_or(true, |e| e >= USER_APP_MAX_SIZE)
        || (flags & !(MREMAP_MAYMOVE | MREMAP_FIXED)) != 0
    {
        return EINVAL.as_isize();
    }
    if flags & MREMAP_FIXED != 0 {
        // 固定地址重映射暂不支持
        return EINVAL.as_isize();
    }

    let old_sz = (old_size + page - 1) & !(page - 1);
    let new_sz = (new_size + page - 1) & !(page - 1);
    if old_sz == new_sz {
        return old_addr as isize;
    }

    // 缩小：解除尾部映射
    if new_sz < old_sz {
        if mmap::do_munmap(old_addr + new_sz, old_sz - new_sz).is_ok() {
            return old_addr as isize;
        }
        return EINVAL.as_isize();
    }

    // 扩大：优先原地扩展
    {
        let task = current_task().unwrap();
        let mm = task
            .inner_exclusive_access()
            .mm
            .as_ref()
            .cloned()
            .ok_or(EINVAL.as_isize());
        if let Ok(mm) = mm {
            let ret = mm.write().mremap_inplace(old_addr, old_sz, new_sz);
            if let Ok(addr) = ret {
                return addr as isize;
            }
        }
    }

    if flags & MREMAP_MAYMOVE == 0 {
        return ENOMEM.as_isize();
    }

    // 移动：新映射 + 拷贝 + 解除旧映射
    let new_addr = match mmap::do_mmap(
        0,
        new_sz,
        mmap::MMapProt::PROT_READ | mmap::MMapProt::PROT_WRITE,
        mmap::MMapFlags::MAP_PRIVATE | mmap::MMapFlags::MAP_ANONYMOUS,
        None,
        0,
    ) {
        Ok(addr) => addr,
        Err(errno) => return errno,
    };

    let copy_len = old_sz;
    let token = current_user_token();
    let src = crate::mm::page_table::translated_byte_buffer(token, old_addr as *const u8, copy_len);
    let dst = crate::mm::page_table::translated_byte_buffer_mut(
        token,
        new_addr as *mut u8,
        copy_len,
    );
    let mut copied = 0usize;
    for (s, d) in src.into_iter().zip(dst.into_iter()) {
        let c = core::cmp::min(s.len(), d.len());
        d[..c].copy_from_slice(&s[..c]);
        copied += c;
        if copied >= copy_len {
            break;
        }
    }

    mmap::do_munmap(old_addr, old_sz).ok();
    new_addr as isize
}

// 回写内存映射区域到文件
// 目前的实现回写的包括非共享页缓存
pub fn sys_msync(_addr: usize, _len: usize, _flags: u32) -> isize {
    crate::mm::mmap::sync_shared_page_cache();
    0
}

/// mincore - 查询虚拟地址范围内各页当前是否驻留。
///
/// 匿名 mmap 采用惰性分配，因此只有已经建立有效 PTE 的页返回驻留位 1；
/// 地址范围仍必须完整落在现有 VMA 中，否则按 Linux 语义返回 ENOMEM。
pub fn sys_mincore(addr: usize, len: usize, vec: *mut u8) -> isize {
    if addr % PAGE_SIZE != 0 {
        return EINVAL.as_isize();
    }
    if len == 0 {
        return 0;
    }
    let Some(end) = addr.checked_add(len) else {
        return ENOMEM.as_isize();
    };
    if end > USER_APP_MAX_SIZE || vec.is_null() {
        return if vec.is_null() {
            EFAULT.as_isize()
        } else {
            ENOMEM.as_isize()
        };
    }

    let page_count = len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
    let task = current_task().unwrap();
    let mm = {
        let inner = task.inner_exclusive_access();
        let Some(mm) = inner.mm.as_ref() else {
            return ENOMEM.as_isize();
        };
        mm.clone()
    };
    let residency = {
        let memory = mm.read();
        let mut result = alloc::vec::Vec::with_capacity(page_count);
        for index in 0..page_count {
            let page_addr = addr + index * PAGE_SIZE;
            let vpn = crate::mm::VirtAddr::from(page_addr).std_floor();
            if !memory.areas().iter().any(|area| area.contains(vpn)) {
                return ENOMEM.as_isize();
            }
            let resident = memory
                .translate(vpn)
                .map(|pte| pte.is_valid())
                .unwrap_or(false);
            result.push(resident as u8);
        }
        result
    };

    let token = current_user_token();
    for (index, resident) in residency.into_iter().enumerate() {
        if !crate::mm::try_translated_write(token, unsafe { vec.add(index) }, resident) {
            return EFAULT.as_isize();
        }
    }
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
            // Discard physical pages without removing the virtual mapping.
            match mmap::do_madvise_dontneed(addr, len) {
                Ok(()) => 0,
                Err(errno) => errno,
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
