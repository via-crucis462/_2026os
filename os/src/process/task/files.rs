use crate::fs::{File, Stderr, Stdin, Stdout};
use alloc::{sync::Arc, vec::Vec};

bitflags::bitflags! {
    pub struct FdFlags: usize {
        const CLOEXEC  = 0o2000000;
        const NONBLOCK = 0o4000;
    }
}
#[derive(Clone)]
pub struct FileDescriptorTable { pub fds: Vec<FileDescriptor>, pub next_fd: usize }
impl FileDescriptorTable {
    pub const DEFAULT_LIMIT: usize = 1024;
    pub fn empty() -> Self { Self { fds: Vec::new(), next_fd: 0 } }
    pub fn new() -> Self {
        let mut fds = Vec::new();
        fds.push(FileDescriptor::new(Arc::new(Stdin), FdFlags::empty(), 0));
        fds.push(FileDescriptor::new(Arc::new(Stdout), FdFlags::empty(), 0));
        fds.push(FileDescriptor::new(Arc::new(Stderr), FdFlags::empty(), 0));
        Self { fds, next_fd: 3 }
    }
    pub fn alloc_fd(&mut self, nofile_limit: usize) -> Option<usize> {
        let effective_limit = nofile_limit.min(Self::DEFAULT_LIMIT);
        let search_end = self.fds.len().min(effective_limit);
        if let Some(fd) = (0..search_end).find(|fd| self.fds[*fd].is_available()) { self.fds[fd] = FileDescriptor::reserved(); return Some(fd); }
        if self.fds.len() >= effective_limit { return None; }
        let fd = self.fds.len(); self.fds.push(FileDescriptor::reserved()); Some(fd)
    }
    pub fn ensure_slots(&mut self, target_len: usize, nofile_limit: usize) -> bool {
        if target_len > nofile_limit.min(Self::DEFAULT_LIMIT) { return false; }
        while self.fds.len() < target_len { self.fds.push(FileDescriptor::empty()); }
        true
    }
    pub fn set_fd(&mut self, fd: usize, file: Arc<dyn File + Send + Sync>, flags: FdFlags, status: usize) { debug_assert!(fd < self.fds.len()); self.fds[fd] = FileDescriptor::new(file, flags, status); }
    pub fn clear_fd(&mut self, fd: usize) { self.fds[fd] = FileDescriptor::empty(); }
}
#[derive(Clone)]
pub struct FileDescriptor { pub file: Option<Arc<dyn File + Send + Sync>>, pub flags: FdFlags, pub status: usize }
const FD_STATUS_RESERVED: usize = 1usize << (usize::BITS as usize - 1);
impl FileDescriptor {
    pub fn empty() -> Self { Self { file: None, flags: FdFlags::empty(), status: 0 } }
    pub fn new(file: Arc<dyn File + Send + Sync>, flags: FdFlags, status: usize) -> Self { Self { file: Some(file), flags, status: status & !FD_STATUS_RESERVED } }
    pub fn reserved() -> Self { Self { file: None, flags: FdFlags::empty(), status: FD_STATUS_RESERVED } }
    pub fn is_available(&self) -> bool { self.file.is_none() && (self.status & FD_STATUS_RESERVED) == 0 }
}