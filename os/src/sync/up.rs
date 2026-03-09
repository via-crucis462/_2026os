//! 多核安全数据管理器
use spin::{Mutex, MutexGuard};

/// Wrap a static data structure inside it so that we are
/// able to access it without any `unsafe`.
///
/// 可以多核访问
///
/// In order to get mutable reference of inner data, call
/// `exclusive_access`.
pub struct MPSafeCell<T> {
    /// inner data
    inner: Mutex<T>,
}

unsafe impl<T> Sync for MPSafeCell<T> {}

impl<T> MPSafeCell<T> {
    // 现已支持多核
    pub fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }
    /// 当数据已经被其他线程访问时，调用此函数会忙等待，直到数据可用
    pub fn exclusive_access(&self) -> MutexGuard<'_, T> {
        self.inner.lock()
    }
}
