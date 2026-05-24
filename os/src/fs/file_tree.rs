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
    /// 将self作为起点，不考虑路径是否以'/'开头
    pub fn find_tree(self: &Arc<Self>, path: &str, follow_links: bool) -> Option<Arc<Dentry>> {
        if path.is_empty() {
            return Some(self.clone());
        }

        // 1. 确定搜索起点
        let mut current = if path.starts_with('/') {
            ROOT_DENTRY.clone()
        } else {
            self.clone()
        };

        // 2. 将路径拆解为动态组件队列 (忽略 ".")
        let mut components: alloc::vec::Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(String::from)
            .collect();

        let mut symlink_depth = 0;
        const MAX_SYMLINK_DEPTH: usize = 8; // 最大软链接解析深度

        // 3. 核心迭代解析循环
        while !components.is_empty() {
            let comp = components.remove(0); // 取出当前要解析的层级

            // 处理上一级目录 ".."
            if comp == ".." {
                if let Some(parent) = current.parent.upgrade() {
                    current = parent;
                }
                continue;
            }

            // 查找子节点（利用你写好的带缓存的 find_child）
            let next = current.find_child(&comp)?;

            // 检查是不是软链接
            let stat = next.inode.get_stat();
            let is_symlink = (stat.mode & 0o170000) == 0o120000; // S_IFLNK

            if is_symlink {
                let is_last_segment = components.is_empty();
                
                // 如果是最后一个路径分量且不需要追踪链接（对应 O_NOFOLLOW），直接返回软链接的 Dentry
                if is_last_segment && !follow_links {
                    current = next;
                    break;
                }

                // 深度检查，防止 A -> B -> A 死循环炸掉内核栈
                symlink_depth += 1;
                if symlink_depth > MAX_SYMLINK_DEPTH {
                    warn!("[VFS] find_tree: ELOOP (Too many levels of symbolic links) path='{}'", path);
                    return None;
                }

                // 读取软链接指向的目标路径
                let size = stat.size as usize;
                let mut buffer = alloc::vec![0u8; size];
                let read_len = next.inode.read_at(0, &mut buffer);
                let target_path = alloc::string::String::from_utf8_lossy(&buffer[..read_len]).into_owned();

                // 如果软链接目标是绝对路径，起点直接切回根目录
                if target_path.starts_with('/') {
                    current = ROOT_DENTRY.clone();
                }

                // 把软链接目标拆解，作为新的路径前缀塞入队列
                let mut new_comps: alloc::vec::Vec<String> = target_path
                    .split('/')
                    .filter(|s| !s.is_empty() && *s != ".")
                    .map(String::from)
                    .collect();

                // 原有剩下的路径接在展开的软链接后面
                new_comps.extend(components);
                components = new_comps;

            } else {
                // 普通文件或目录，正常步进
                current = next;
            }
        }

        Some(current)
    }

    /// 查找子节点（单级）：返回的是 Dentry 包装，以便继续向下查找
    pub fn find_child(self: &Arc<Self>, name: &str) -> Option<Arc<Dentry>> {
        trace!("[kernel] Dentry::find_child: parent={}, name={}", self.name, name);
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
        trace!("VFS: File '{}' not found in directory '{}'", name, self.name);
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
        let ext4fs = crate::ext4fs::ext4::Ext4FS::open(crate::drivers::BLOCK_DEVICE.clone());
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
        }else{
            String::from(&path[..pos])
        }   
    } else {
        String::from(".")
    }
}

pub fn file_name(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return String::from("/");
    }
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
