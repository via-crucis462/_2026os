use super::{create_file_in_dentry, Dentry, File, OSInode, Pipe, Stat};
use crate::auth::PermStat;
use crate::mm::UserBuffer;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use lazy_static::lazy_static;
use spin::Mutex;

pub const S_IFIFO: u32 = 0o010000;
pub const S_IFMT: u32 = 0o170000;

lazy_static! {
	static ref NAMED_PIPES: Mutex<BTreeMap<u64, (Arc<Pipe>, Arc<Pipe>)>> = Mutex::new(BTreeMap::new());
}

struct NamedPipeDuplex {
	read_end: Arc<Pipe>,
	write_end: Arc<Pipe>,
}

impl File for NamedPipeDuplex {
	fn readable(&self) -> bool { true }
	fn writable(&self) -> bool { true }
	fn read(&self, buf: UserBuffer) -> usize { self.read_end.read(buf) }
	fn write(&self, buf: UserBuffer) -> usize { self.write_end.write(buf) }
	fn write_nonblock(&self, buf: UserBuffer) -> Result<usize, crate::syscall::errno::Errno> {
		self.write_end.write_nonblock(buf)
	}
	fn read_at(&self, _offset: usize, buf: UserBuffer) -> usize { self.read_end.read(buf) }
	fn write_at(&self, _offset: usize, buf: UserBuffer) -> usize { self.write_end.write(buf) }
	fn get_perm(&self) -> PermStat { self.read_end.get_perm() }
	fn get_stat(&self) -> Stat { self.read_end.get_stat() }
	fn getdents(&self, _buf: &mut [u8]) -> isize { crate::syscall::errno::Errno::EINVAL.as_isize() }
	fn ready_to_read(&self) -> bool { self.read_end.ready_to_read() }
	fn ready_to_write(&self) -> bool { self.write_end.ready_to_write() }
	fn check_write_error(&self) -> Option<crate::syscall::errno::Errno> { self.write_end.check_write_error() }
	fn as_any(&self) -> &dyn Any { self }
}

fn ensure_named_pipe(ino: u64) -> (Arc<Pipe>, Arc<Pipe>) {
	let mut pipes = NAMED_PIPES.lock();
	pipes.entry(ino).or_insert_with(super::make_pipe).clone()
}

pub fn is_fifo_mode(mode: u32) -> bool {
	(mode & S_IFMT) == S_IFIFO
}

pub fn create_fifo_in_dentry(parent: &Arc<Dentry>, name: String, mode: u32) -> Arc<Dentry> {
	let dentry = create_file_in_dentry(parent, name, mode);
	ensure_named_pipe(dentry.inode.get_stat().ino);
	dentry
}

pub fn open_fifo_file(inode: &OSInode, readable: bool, writable: bool) -> Arc<dyn File> {
	let ino = inode.inode.get_stat().ino;
	let (read_end, write_end) = ensure_named_pipe(ino);
	match (readable, writable) {
		(true, false) => read_end,
		(false, true) => write_end,
		(true, true) | (false, false) => Arc::new(NamedPipeDuplex { read_end, write_end }),
	}
}
