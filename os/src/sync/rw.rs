//! 写者优先的自旋读写锁。
//!
//! 语义：读者之间共享；写者独占；一旦有写者开始等待（`asked_write > 0`），
//! 新读者不再进入，直到所有已排队写者完成，避免写者饥饿。
//! 适用于内核中“读多写少但写者不能饿死”的临界区（如 mm 的 areas 结构）。

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicIsize, Ordering};

pub struct RwLock<T: ?Sized> {
    /// 读者数量（>=0），或 -1 表示写者持有
    state: AtomicIsize,
    /// 正在等待/已排队的写者数量
    asked_write: AtomicIsize,
    inner: UnsafeCell<T>,
}

// 与 std::sync::RwLock 相同的约束：T 需要 Send 才能跨线程移动锁，
// 需要 Send + Sync 才能共享引用。
unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    #[inline]
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicIsize::new(0),
            asked_write: AtomicIsize::new(0),
            inner: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// 获取共享读锁。若已有写者持有或有写者排队，则自旋等待。
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state >= 0 && self.asked_write.load(Ordering::Acquire) == 0 {
                if self
                    .state
                    .compare_exchange(state, state + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    // 严格写者优先：CAS 成功后再次确认没有写者插队，
                    // 避免“检查通过后、CAS 前”的 TOCTOU 窗口。
                    if self.asked_write.load(Ordering::Acquire) == 0 {
                        return RwLockReadGuard {
                            lock: self,
                            _marker: PhantomData,
                        };
                    }
                    // 写者在 CAS 成功后已排队：退回读者计数并重试。
                    self.state.fetch_sub(1, Ordering::AcqRel);
                }
            } else {
                core::hint::spin_loop();
            }
        }
    }

    /// 获取独占写锁。先登记排队，再等待读者归零。
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.asked_write.fetch_add(1, Ordering::AcqRel);
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state == 0 {
                if self
                    .state
                    .compare_exchange(0, -1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.asked_write.fetch_sub(1, Ordering::AcqRel);
                    return RwLockWriteGuard {
                        lock: self,
                        _marker: PhantomData,
                    };
                }
            } else {
                core::hint::spin_loop();
            }
        }
    }
}

pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    _marker: PhantomData<&'a T>,
}

// 读者可以在任意线程释放，因此 guard 需要 Send。
unsafe impl<T: ?Sized + Sync> Send for RwLockReadGuard<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for RwLockReadGuard<'_, T> {}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: 持有读锁期间没有写者，可安全共享访问。
        unsafe { &*self.lock.inner.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.state.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    _marker: PhantomData<&'a mut T>,
}

unsafe impl<T: ?Sized + Send + Sync> Send for RwLockWriteGuard<'_, T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLockWriteGuard<'_, T> {}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: 持有写锁期间独占访问。
        unsafe { &*self.lock.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: 持有写锁期间独占访问。
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        // 写者持有期间 state 恒为 -1，直接置 0 即可。
        self.lock.state.store(0, Ordering::Release);
    }
}
