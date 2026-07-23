#[allow(unused)]
use super::File;
use crate::drivers::BLOCK_DEVICE;
use crate::task::current_task;
use alloc::sync::Arc;
use bitflags::*;
use lazy_static::*;
use crate::ext4fs::ext4::Ext4FS;
use crate::ext4fs::ext4inode::Ext4Inode;
use crate::fs::file_tree::*;
use super::VfsInode;
use spin::Mutex;
use crate::mm::UserBuffer;
use crate::fs::TimeSpec;
use crate::auth::PermStat;
use core::any::Any;
use crate::mm::PhysPageNum;


pub struct OSInode {
    readable: bool,
    writable: bool,
    inner: Mutex<OSInodeInner>,
    pub inode: Arc<dyn VfsInode>,   //实现了VfsInode trait的具体文件系统的inode
    pub dentry: Arc<Dentry>, 
}

pub struct OSInodeInner {
    offset: usize,  //记录当前文件的读写偏移量，read/write系统调用会更新这个偏移量
    mounted_offset: usize,
}
// 判断文件类型的函数，返回值对应 Linux dirent 结构体中的 d_type 字段
fn dirent_type_from_mode(mode: u32) -> u8 {
    match mode & 0o170000 {
        0o010000 => 1,  // FIFO / named pipe -> DT_FIFO
        0o020000 => 2,  // character device -> DT_CHR
        0o040000 => 4,  // directory -> DT_DIR
        0o060000 => 6,  // block device -> DT_BLK
        0o100000 => 8,  // regular file -> DT_REG
        0o120000 => 10, // symbolic link -> DT_LNK
        0o140000 => 12, // Unix domain socket -> DT_SOCK
        _ => 0,
    }
}
// 把目录项写到用户缓冲区的函数，返回写了多少字节
fn append_dirent_record(
    buf: &mut [u8],
    buf_offset: usize,
    inode_id: u64,
    next_offset: i64,
    d_type: u8,
    name: &str,
) -> Option<usize> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    let total_len = 19 + name_len + 1;
    let d_reclen = (total_len + 7) & !7;
    if buf_offset + d_reclen > buf.len() {
        return None;
    }

    buf[buf_offset..buf_offset + 8].copy_from_slice(&inode_id.to_ne_bytes());
    buf[buf_offset + 8..buf_offset + 16].copy_from_slice(&next_offset.to_ne_bytes());
    buf[buf_offset + 16..buf_offset + 18].copy_from_slice(&(d_reclen as u16).to_ne_bytes());
    buf[buf_offset + 18] = d_type;
    buf[buf_offset + 19..buf_offset + 19 + name_len].copy_from_slice(name_bytes);
    for byte in &mut buf[buf_offset + 19 + name_len..buf_offset + d_reclen] {
        *byte = 0;
    }
    Some(d_reclen)
}

impl OSInode {
    pub fn new(readable: bool, writable: bool, inode: Arc<dyn VfsInode>, dentry: Arc<Dentry>) -> Self {
        Self {
            readable,
            writable,
            inner: Mutex::new(OSInodeInner { offset: 0, mounted_offset: 0 }),
            inode,
            dentry,
        }
    }
    pub fn set_time(&self, atime: &TimeSpec, mtime: &TimeSpec) -> isize {
        self.inode.set_time(atime, mtime)
    }
    pub fn read_all(&self) -> alloc::vec::Vec<u8> {
        // 1. 获取文件总大小
        let size = self.inode.get_size();
        trace!("[kernel] read_all: size={}", size);
        // 2. 准备缓冲区
        let mut buffer = alloc::vec![0u8; size];
        // 3. 从偏移量 0 开始读取
        let read_len = self.inode.read_at(0, &mut buffer);
        trace!("[kernel] read_all: read_len={}", read_len);
        
        // 理论上 read_len 应该等于 size
        if read_len != size {
            buffer.truncate(read_len);
        }
        buffer
    }
    pub fn get_dentry(&self) -> Arc<Dentry> {
        self.dentry.clone()
    }
}

impl File for OSInode {
    fn info_type(&self) {
        println!("osinode");
    }
    fn readable(&self) -> bool { self.readable }
    fn writable(&self) -> bool { self.writable }

    fn read(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.lock();
        let offset = inner.offset;
        let read_len = self.read_at(offset, buf);
        inner.offset += read_len;
        read_len
    }

    fn write(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.lock();
        let offset = inner.offset;
        let write_len = self.write_at(offset, buf);
        inner.offset += write_len;
        write_len
    }

    fn raw_read_at(&self, offset: usize, mut buf: UserBuffer) -> usize {
        let mut total_read = 0;
        let mut current_offset = offset;
        for slice in buf.buffers.iter_mut() {
            let read_len = self.inode.raw_read_at(current_offset, *slice);
            if read_len == 0 { break; }
            current_offset += read_len;
            total_read += read_len;
        }
        total_read
    }

    fn raw_write_at(&self, offset: usize, buf: UserBuffer) -> usize {
        let mut total_write = 0;
        let mut current_offset = offset;
        for slice in buf.buffers.iter() {
            let write_len = self.inode.raw_write_at(current_offset, *slice);
            if write_len == 0 { break; }
            current_offset += write_len;
            total_write += write_len;
        }
        total_write
    }

    /// 带页缓存的读取，调用 VfsInode::read_at
    fn read_at(&self, offset: usize, mut buf: UserBuffer) -> usize {
        // 注册到全局页缓存管理器，以便周期性回写能找到此文件
        crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER
            .register_vfs_inode(self.inode.ino(), &self.inode);

        let mut total_read = 0;
        let mut current_offset = offset;
        for slice in buf.buffers.iter_mut() {
            let read_len = self.inode.read_at(current_offset, *slice);
            if read_len == 0 { break; }
            current_offset += read_len;
            total_read += read_len;
        }
        total_read
    }

    /// 带页缓存的写入，调用 VfsInode::write_at
    fn write_at(&self, offset: usize, buf: UserBuffer) -> usize {
        // 注册到全局页缓存管理器，以便周期性回写能找到此文件
        crate::mm::mmap::SHARED_PAGE_CACHE_MANAGER
            .register_vfs_inode(self.inode.ino(), &self.inode);

        let mut total_write = 0;
        let mut current_offset = offset;
        for slice in buf.buffers.iter() {
            let write_len = self.inode.write_at(current_offset, *slice);
            if write_len == 0 { break; }
            current_offset += write_len;
            total_write += write_len;
        }
        total_write
    }

    fn get_stat(&self) -> super::Stat {
        self.inode.get_stat()
    }

    fn getdents(&self, buf: &mut [u8]) -> isize{
        let mut inner = self.inner.lock();
        let read_bytes = self.inode.getdents(&mut inner.offset, buf);
        if read_bytes < 0 {
            return read_bytes;
        }

        let lower_size = self.inode.get_size();
        if inner.offset < lower_size {
            return read_bytes;
        }

        let mounted_children = self.dentry.mounted_children_snapshot();
        if mounted_children.is_empty() {
            return read_bytes;
        }

        let mut buf_offset = read_bytes as usize;
        let mut mount_index = inner.mounted_offset;
        while mount_index < mounted_children.len() {
            let child = &mounted_children[mount_index];
            let stat = child.inode.get_stat();
            let next_offset = (lower_size + mount_index + 1) as i64;
            let Some(written) = append_dirent_record(
                buf,
                buf_offset,
                stat.ino,
                next_offset,
                dirent_type_from_mode(stat.mode),
                child.name.as_str(),
            ) else {
                break;
            };
            buf_offset += written;
            mount_index += 1;
        }
        inner.mounted_offset = mount_index;
        buf_offset as isize
    }

    fn get_dentry(&self) -> Option<Arc<super::Dentry>> {
        Some(self.dentry.clone())
    }

    fn pread(&self, offset: usize, buf: UserBuffer) -> usize {
        let read_len = self.read_at(offset, buf);
        read_len
    }

    fn get_perm(&self) -> crate::auth::PermStat {
        self.inode.get_perm()
    }

    fn set_perm(&self, perm: PermStat) -> bool {
        self.inode.set_perm(perm)
    }

    fn lseek(&self, offset: isize, whence: i32) -> isize {
        const SEEK_SET: i32 = 0; // 从文件开头算起
        const SEEK_CUR: i32 = 1; // 从当前位置算起
        const SEEK_END: i32 = 2; // 从文件末尾算起

        let mut inner = self.inner.lock(); 
        
        let current_offset = inner.offset as isize;

        // 2. 根据 whence 计算新的偏移量
        let new_offset = match whence {
            SEEK_SET => offset,
            SEEK_CUR => current_offset + offset,
            SEEK_END => {
                let file_size = self.inode.get_size() as isize; 
                file_size + offset
            },
            _ => return -22, // EINVAL (Invalid argument) whence 参数不合法
        };

        // 3. 偏移量不能移动到文件头部之前（不能是负数）
        if new_offset < 0 {
            return -22; // EINVAL
        }

        // 4. 更新 inode 内部的偏移量
        inner.offset = new_offset as usize;
        if new_offset == 0 {
            inner.mounted_offset = 0;
        }
        
        // 5. 成功返回新的偏移量
        new_offset as isize
    }
    
    fn get_shared_page(&self, page_offset: usize) -> Option<Arc<Mutex<crate::mm::mmap::PageCache>>> {
        // 转发给底层的具体文件系统 Inode
        self.inode.get_shared_page(page_offset)
    }

    fn truncate(&self, len: usize) -> bool {
        self.inode.truncate(len)
    }

    fn as_any(&self) -> &dyn Any { self }
}

bitflags! {
    ///  The flags argument to the open() system call is constructed by ORing together zero or more of the following values:
    pub struct OpenFlags: u32 {
        /// readyonly
        const RDONLY = 0;
        /// writeonly
        const WRONLY = 1 << 0;
        /// read and write
        const RDWR = 1 << 1;
        /// create new file
        const CREATE = 1 << 6;
        
        /// truncate file size to 0
        const TRUNC = 1 << 9;
        /// 非阻塞模式 (O_NONBLOCK)
        const NONBLOCK = 1 << 11;
        /// 用于mkdir中，open二次确认是否新建的文件是目录类型
        const DIRECTORY = 1 << 16;
        /// 不追踪符号链接
        const NOFOLLOW = 1 << 17;
    }
}
impl OpenFlags {
    /// Do not check validity for simplicity
    /// Return (readable, writable)
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {
            (true, false)
        } else if self.contains(Self::WRONLY) {
            (false, true)
        } else {
            (true, true)
        }
    }
    pub fn should_create(&self) -> bool {
        self.contains(Self::CREATE)
    }
    pub fn should_be_directory(&self) -> bool {
        self.contains(Self::DIRECTORY)
    }
    pub fn is_nofollow(&self) -> bool {
        self.contains(Self::NOFOLLOW)
    }
}
pub fn open_file(base: Arc<Dentry>,path: &str, flags: OpenFlags, mode: u32) -> Option<Arc<OSInode>> {
    warn!("VFS: open_file - path='{}', flags={:?},cwd={}", path, flags, base.name);
    let start_node = if path.starts_with('/') {
        ROOT_DENTRY.clone() // 绝对路径，从根开始
    } else {
        base // 相对路径，从 base 开始
    };
    
    // 使用全局 Dentry 树递归查找路径，并自动填充缓存
    // 1. 查找文件是否已存在
    let target_dentry = if let Ok(dentry) = start_node.find_tree(path, !flags.is_nofollow()) {
        Some(dentry)
    } else {
        None
    };
    // 2.1 若不存在
    // 2.1.1 若文件不需要创建，返回 None
    if target_dentry.is_none() {
        // 文件不存在，且没有创建标志，返回 None
        if !flags.should_create() {
            return None;
        }
        // 创建新文件的逻辑（简化处理，只创建空文件）
    // 2.1.2 创建新文件
        let parent_path = parent_path(path);
        let Ok(parent_dentry) = start_node.find_tree(&parent_path, true) else {
            return None;
        };
        let file_name = file_name(path);
        let new_dentry = create_file_in_dentry(&parent_dentry, file_name, mode);
        let (readable, writable) = flags.read_write();
        return Some(Arc::new(OSInode::new(
            readable,
            writable,
            new_dentry.inode.clone(),
            new_dentry,
        )));
    }
    // 2.2 若存在，直接返回对应的 OSInode
    let target_dentry = target_dentry.unwrap();
    let (readable, writable) = flags.read_write();
    Some(Arc::new(OSInode::new(
        readable,
        writable,
        target_dentry.inode.clone(),
        target_dentry,
    )))
}

pub fn make_dir(path: &str , _mode: u32) -> Option<u32> {
    // 获取目标路径的起点
    let start = if path.starts_with('/') {
        ROOT_DENTRY.clone()
    } else {
        let task = current_task().unwrap();
        let fs = task.inner_exclusive_access().fs.clone();
        let fs = fs.exclusive_access();
        fs.get_pwd()
    };
    // 从起点开始检查目标路径是否已存在
    if let Ok(_) = start.find_tree(path, true) {
        info!("VFS: make_dir - target '{}' already exists", path);
        return None; 
    }
    let parent_path = parent_path(path);
    let Ok(parent_dentry) = start.find_tree(&parent_path, true) else {
        info!("VFS: make_dir - parent path '{}' does not exist", parent_path);
        return None;
    };
    let dir_name = file_name(path);
    info!("VFS: make_dir - creating directory '{}' in parent '{}'", dir_name, parent_path);
    let new_dentry = create_dir_in_dentry(&parent_dentry, dir_name , _mode);
    Some(new_dentry.inode.get_stat().ino as u32)
}

/// List all apps in the root directory
pub fn list_apps() {
    println!("/**** APPS ****");
    let mut buf = [0u8; 4096];
    let mut file_offset = 0;
    let len = ROOT_INODE.inode.getdents(&mut file_offset,&mut buf);
    if len > 0 {
        let mut offset = 0;
        while offset < len as usize {
            let entry = unsafe { &*(buf[offset..].as_ptr() as *const super::DirEntry) };
            if entry.d_reclen == 0 { break; }
            
            let name_len = entry.d_name.iter().position(|&c| c == 0).unwrap_or(256);
            let name = core::str::from_utf8(&entry.d_name[..name_len]).unwrap_or("");
            println!("{}", name);
            
            offset += entry.d_reclen as usize;
        }
    }
    println!("**************/");
}
lazy_static! {
    pub static ref ROOT_VFS_INODE: Arc<dyn VfsInode> = {
        let ext4fs = Ext4FS::open(BLOCK_DEVICE.clone());
        let root_disk_inode = ext4fs.get_disk_inode(2);
        Arc::new(Ext4Inode::new(2, &root_disk_inode, Arc::new(ext4fs), None))
    };

    pub static ref ROOT_INODE: Arc<OSInode> = {
        Arc::new(OSInode::new(true, false, ROOT_VFS_INODE.clone(), ROOT_DENTRY.clone()))
    };
}
