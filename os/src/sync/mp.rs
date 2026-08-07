//! 多核安全数据管理器
//! 保留这个的目的是避免修改原有 UPSafeCell 包装
//! 本质上是 spin::Mutex
//! 
//! 注: 后续实现实际上应该尽可能用 spin::Mutex 代替 MPSafeCell

#[cfg(target_arch = "riscv64")]
use riscv::register::sstatus;
use super::*;

/// Wrap a static data structure inside it so that we are
/// able to access it without any `unsafe`.
///
/// 可以多核访问
///
/// In order to get mutable reference of inner data, call
/// `exclusive_access`.

use spin::{Mutex, MutexGuard};

const LOCK_WAIT_WARN_AFTER_US: usize = 1_000_000;
const LOCK_WAIT_WARN_INTERVAL_US: usize = 5_000_000;

pub struct MPSafeCell<T> {
    /// inner data
    inner: Mutex<T>,
}

impl<T> MPSafeCell<T> {
    // 现已支持多核
    pub fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }
    /// 当数据已经被其他线程访问时，调用此函数会忙等待，直到数据可用。
    /// 持续等待时定期输出诊断，帮助定位可能的死锁或长时间持锁。
    pub fn exclusive_access(&self) -> MutexGuard<'_, T> {
        let wait_started_us = crate::arch::timer::get_time_us();
        let mut next_report_us = wait_started_us.saturating_add(LOCK_WAIT_WARN_AFTER_US);

        loop {
            if let Some(guard) = self.inner.try_lock() {
                return guard;
            }

            let now_us = crate::arch::timer::get_time_us();
            if now_us >= next_report_us {
                println!(
                    "[MPSafeCell] lock wait cell={:p} hart={} waited={}us",
                    self as *const Self,
                    crate::get_hart_id(),
                    now_us.saturating_sub(wait_started_us),
                );
                next_report_us = now_us.saturating_add(LOCK_WAIT_WARN_INTERVAL_US);
            }
            core::hint::spin_loop();
        }
    }
    pub fn get_mutex(&self) -> &Mutex<T> {
        &self.inner
    }
}

pub type MPSafeGuard<'a, T> = MutexGuard<'a, T>;


/* 废弃，内核态不安全
pub struct MPSafeCell<T> {
    /// inner data
    inner: Semaphore<T>,
}

unsafe impl<T> Sync for MPSafeCell<T> {}

impl<T> MPSafeCell<T> {
    // 现已支持多核
    pub fn new(value: T) -> Self {
        Self {
            inner: Semaphore::new(1,value),
        }
    }
    /// 当数据已经被其他线程访问时，调用此函数会忙等待，直到数据可用
    pub fn exclusive_access(&self) -> MPSafeGuard<'_, T> {
        self.inner.lock()
    }
}

pub type MPSafeGuard<'a, T> = SemaphoreGuard<'a, T>;
*/