use alloc::sync::{Arc, Weak};
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::*;
use super::{VfsInode};

pub struct Dentry {
    pub name: String,
    pub inode: Arc<dyn VfsInode>,
    pub parent: Weak<Dentry>,
    pub children: Mutex<BTreeMap<String, Arc<Dentry>>>,
}

impl Dentry {
    pub fn new(
        name: String,
        inode: Arc<dyn VfsInode>,
        parent: Weak<Dentry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            inode,
            parent,
            children: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn find(self: &Arc<Self>, name: &str) -> Arc<dyn VfsInode> {
        // 1. 尝试从当前节点的缓存中获取
        let mut children = self.children.lock();
        if let Some(child) = children.get(name) {
            return child.inode.clone();
        }

        // 2. 缓存未击中，调用底层磁盘接口查找
        // 注意：这里调用 self.inode.find 是 VfsInode 特征定义的磁盘查找接口
        if let Some(vfs_inode) = self.inode.find(name) {
            // 找到了，将其包装成 Dentry 并插入缓存树
            let new_child = Self::new(
                String::from(name),
                vfs_inode.clone(),
                Arc::downgrade(self),
            );
            children.insert(String::from(name), new_child);
            return vfs_inode;
        }

        // 3. 磁盘也没找到，按照要求 panic
        panic!("VFS: File '{}' not found in directory '{}'", name, self.name);
    }
    pub fn insert(self: &Arc<Self>, name: String, inode: Arc<dyn VfsInode>) -> Arc<Self> {
        let mut children = self.children.lock();
        if let Some(child) = children.get(&name) {
            return child.clone();
        }
        
        // 创建新节点，并将 parent 指向当前节点（self）
        let new_child = Self::new(
            name.clone(),
            inode,
            Arc::downgrade(self), // 使用 Weak 防止循环引用
        );
        
        children.insert(name, new_child.clone());
        new_child
    }
    /// 递归查找完整路径，例如 "bin/sh" 或 "/bin/sh"
    pub fn find_tree(self: &Arc<Self>, path: &str) -> Option<Arc<Dentry>> {
        if path == "." || path == "" {
            return Some(self.clone());
        }
        let segments: alloc::vec::Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        
        let mut current = self.clone();
        for seg in segments {
            if seg == "." {
                continue;
            } else if seg == ".." {
                if let Some(parent) = current.parent.upgrade() {
                    current = parent;
                }
                // 如果没有 parent，说明是根目录，则保持不变
                continue;
            }
            current = current.find_child(seg)?;
        }
        Some(current)
    }

    /// 查找子节点（单级）：返回的是 Dentry 包装，以便继续向下查找
    pub fn find_child(self: &Arc<Self>, name: &str) -> Option<Arc<Dentry>> {
        trace!("[kernel] Dentry::find_child: name={}", name);
        let mut children = self.children.lock();
        // 1. 尝试从当前节点的缓存中获取
        if let Some(child) = children.get(name) {
            return Some(child.clone());
        }

        // 2. 缓存未击中，调用底层磁盘接口查找
        if let Some(vfs_inode) = self.inode.find(name) {
            // 找到了，将其包装成 Dentry 并插入缓存树
            let new_child = Self::new(
                String::from(name),
                vfs_inode.clone(),
                Arc::downgrade(self),
            );
            children.insert(String::from(name), new_child.clone());
            return Some(new_child);
        }

        // 3. 磁盘也没找到，按照要求 panic
        println!("VFS: File '{}' not found in directory '{}'", name, self.name);
        None
    }

    pub fn get_full_path(self: &Arc<Self>) -> String {
        let mut parts = alloc::vec::Vec::new();
        let mut current = self.clone();

        // 向上回溯直到根目录（根目录的 parent.upgrade() 会返回 None）
        while let Some(parent) = current.parent.upgrade() {
            parts.push(current.name.clone());
            current = parent;
        }

        // 如果 parts 为空，说明当前就是根目录 "/"
        if parts.is_empty() {
            return String::from("/");
        }

        // 将收集到的名字反转并拼接
        let mut full_path = String::new();
        for name in parts.iter().rev() {
            full_path.push('/');
            full_path.push_str(name);
        }
        full_path
    }
}

lazy_static! {
    pub static ref ROOT_DENTRY: Arc<Dentry> = {
        let ext4fs = crate::ext4fs::ext4::Ext4FS::open(crate::drivers::BLOCK_DEVICE);
        let root_disk_inode = ext4fs.get_disk_inode(2); 
        let vfs_inode = Arc::new(crate::ext4fs::ext4inode::Ext4Inode::new(
            2, 
            &root_disk_inode, 
            Arc::new(ext4fs), 
            None
        ));

        Dentry::new(
            String::from("/"),
            vfs_inode,
            Weak::new(),
        )
    };
}

pub fn parent_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if let Some(pos) = path.rfind('/') {
        if pos == 0 {
            String::from("/")
        } else {
            String::from(&path[..pos])
        }
    } else {
        String::from(".")
    }
}

pub fn file_name(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if let Some(pos) = path.rfind('/') {
        String::from(&path[pos + 1..])
    } else {
        String::from(path)
    }
}

pub fn create_file_in_dentry(parent: &Arc<Dentry>, name: String) -> Arc<Dentry> {
    // 默认创建普通文件权限 0o100666
    let vfs_inode = parent.inode.create_file(&name, 0o100666)
        .expect("VFS: Failed to create file in disk");
    
    // 将新创建的 Inode 插入 Dentry 缓存树
    parent.insert(name, vfs_inode)
}

pub fn create_dir_in_dentry(parent: &Arc<Dentry>, name: String, _mode: u32) -> Arc<Dentry> {
    // 解码权限：取 _mode 的低 9 位（权限位）并加上目录类型标志 0o040000 (S_IFDIR)
    let mode = (_mode & 0o777) | 0o040000;
    let vfs_inode = parent.inode.create_dir(&name, mode)
        .expect("VFS: Failed to create directory in disk");
    
    // 将新创建的 Inode 插入 Dentry 缓存树
    parent.insert(name, vfs_inode)
}
