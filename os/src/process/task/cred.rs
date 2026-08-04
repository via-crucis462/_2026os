#[derive(Clone)]
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
impl Cred{
    pub fn new(uid: u32, gid: u32, euid: u32, egid: u32, suid: u32, sgid: u32, fsuid: u32, fsgid: u32) -> Self{
        Self{
            uid,
            gid,
            euid,
            egid,
            suid,
            sgid,
            fsuid,
            fsgid,
        }
    }

    pub fn euid(&self) -> u32 {
        self.euid
    }

    pub fn egid(&self) -> u32 {
        self.egid
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn sgid(&self) -> u32 {
        self.sgid
    }

    pub fn suid(&self) -> u32 {
        self.suid
    }

    pub fn fsuid(&self) -> u32 {
        self.fsuid
    }

    pub fn fsgid(&self) -> u32 {
        self.fsgid
    }

    pub fn set_ruid(&mut self, uid: u32) {
        self.uid = uid;
    }

    pub fn set_uid(&mut self, uid: u32) {
        self.uid = uid;
        self.euid = uid;
        self.fsuid = uid;
    }

    pub fn set_gid(&mut self, gid: u32) {
        self.gid = gid;
        self.egid = gid;
        self.fsgid = gid;
    }

    pub fn set_euid(&mut self, euid: u32) {
        self.euid = euid;
        self.fsuid = euid;
    }

    pub fn set_fsuid(&mut self, fsuid: u32) {
        self.fsuid = fsuid;
    }

    pub fn set_fsgid(&mut self, fsgid: u32) {
        self.fsgid = fsgid;
    }
}
