#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
pub enum Errno {
    EPERM = 1,      /* Operation not permitted */
    ENOENT = 2,     /* No such file or directory */
    ESRCH = 3,      /* No such process */
    EINTR = 4,      /* Interrupted system call */
    EIO = 5,        /* I/O error */
    ENXIO = 6,      /* No such device or address */
    E2BIG = 7,      /* Argument list too long */
    ENOEXEC = 8,    /* Exec format error */
    EBADF = 9,      /* Bad file number */
    ECHILD = 10,    /* No child processes */
    EAGAIN = 11,    /* Try again */
    ENOMEM = 12,    /* Out of memory */
    EACCES = 13,    /* Permission denied */
    EFAULT = 14,    /* Bad address */
    EBUSY = 16,     /* Device or resource busy */
    EEXIST = 17,    /* File exists */
    EXDEV = 18,     /* Cross-device link */
    ENODEV = 19,    /* No such device */
    ENOTDIR = 20,   /* Not a directory */
    EISDIR = 21,    /* Is a directory */
    EINVAL = 22,    /* Invalid argument */
    ENFILE = 23,    /* File table overflow */
    EMFILE = 24,    /* Too many open files */
    ENOSPC = 28,    /* No space left on device */
    ESPIPE = 29,    /* Illegal seek */
    EROFS = 30,     /* Read-only file system */
    EMLINK = 31,    /* Too many links */
    EPIPE = 32,     /* Broken pipe */
    ENOSYS = 38,    /* Invalid system call number */
}

impl Errno {
    /// 转换为系统调用返回的负数值
    pub fn as_isize(&self) -> isize {
        (*self as isize) * -1
    }
}