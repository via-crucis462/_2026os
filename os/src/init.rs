use alloc::string::ToString;
use lazy_static::lazy_static;
use alloc::sync::Arc;
use crate::fs::{ROOT_DENTRY, tmpfs::*};

lazy_static!(
    pub static ref BUSYBOX_PATH: &'static str = "/init/busybox\0";
    pub static ref BUSYBOX_DATA: &'static [u8] = include_bytes!("../../boot/busybox");
    pub static ref INIT_DIR: Arc<TmpfsDirInode> = Arc::new(TmpfsDirInode::new(0o777));
    pub static ref BUSYBOX_INODE: Arc<TmpfsFileInode> = Arc::new(TmpfsFileInode::new_with_data(&BUSYBOX_DATA));
);

pub fn mount_tmp_busybox() {
    let init_dentry = ROOT_DENTRY.mount_child("init".to_string(), INIT_DIR.clone());
    init_dentry.mount_child("busybox".to_string(), BUSYBOX_INODE.clone());
}
