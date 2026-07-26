use crate::fs::Dentry;
use alloc::sync::Arc;

#[derive(Clone)]
pub struct FsStruct {
    root: Arc<Dentry>,
    pwd: Arc<Dentry>,
    umask: u32,
}
impl FsStruct {
    pub fn new(root: Arc<Dentry>, pwd: Arc<Dentry>) -> Self {
        Self { root, pwd, umask: 0o022 }
    }
    pub fn get_root(&self) -> Arc<Dentry> { self.root.clone() }
    pub fn get_pwd(&self) -> Arc<Dentry> { self.pwd.clone() }
    pub fn set_pwd(&mut self, pwd: Arc<Dentry>) { self.pwd = pwd; }
    pub fn umask(&self) -> u32 { self.umask }
    pub fn set_umask(&mut self, umask: u32) -> u32 {
        let old = self.umask;
        self.umask = umask & 0o777;
        old
    }
}
