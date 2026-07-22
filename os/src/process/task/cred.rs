pub struct Cred{
    uid: u32, // 用户ID
    gid: u32, // 组ID
    euid: u32, // 有效用户ID
    egid: u32, // 有效组ID
    suid: u32, // 保存的用户ID
    sgid: u32, // 保存的组ID
    fsuid: u32, // 文件系统用户ID
    fsgid: u32, // 文件系统组ID
}
