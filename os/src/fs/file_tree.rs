use alloc::sync::{Arc, Weak};
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::*;
use super::{VfsInode,ROOT_INODE};

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
    pub fn find_tree(self: &Arc<Self>, path: &str) -> Arc<Dentry> {
        let segments: alloc::vec::Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        
        let mut current = self.clone();
        for seg in segments {
            // 这里我们复用之前的查找逻辑，为了能链式查找，我们先定义一个返回 Arc<Dentry> 的辅助方法
            current = current.find_child(seg);
        }
        current
    }

    /// 查找子节点（单级）：返回的是 Dentry 包装，以便继续向下查找
    pub fn find_child(self: &Arc<Self>, name: &str) -> Arc<Dentry> {
        let mut children = self.children.lock();
        // 1. 尝试从当前节点的缓存中获取
        if let Some(child) = children.get(name) {
            return child.clone();
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
            return new_child;
        }

        // 3. 磁盘也没找到，按照要求 panic
        panic!("VFS: File '{}' not found in directory '{}'", name, self.name);
    }
}

lazy_static! {
    pub static ref ROOT_DENTRY: Arc<Dentry> = {
        Dentry::new(
            String::from("/"),
            ROOT_INODE.inode.clone(),
            Weak::new(),
        )
    };
}
