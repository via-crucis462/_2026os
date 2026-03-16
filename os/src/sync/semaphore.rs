//! 信号量互斥锁实现
use core::cell::UnsafeCell;
use core::ops::Deref;

use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use crate::process::*;
use core::marker::PhantomData;
use core::ops::DerefMut;


pub struct Semaphore<T> {
    count: UnsafeCell<isize>,
    wait_queue: Arc<WaitQueue>,
    inner: T,
}

impl<T> Semaphore<T> {
    pub fn new(count: isize, data: T) -> Self {
        Self {
            count: UnsafeCell::new(count),
            wait_queue: Arc::new(WaitQueue { queue: VecDeque::new() }),
            inner: data,
        }
    }
    pub fn lock(&mut self) -> SemaphoreGuard<'_, T>{
        *self.count.get_mut() -= 1;

        while unsafe{*self.count.get() < 0} {
            // 当前线程进入等待队列
            current_task_to_sleep(self.wait_queue.clone());
        }
        SemaphoreGuard {
            wait_queue: self.wait_queue.clone(),
            count: &self.count,
            data: &self.inner as *const T as *mut T,
        }
    }
}

struct WaitQueue {
    queue: VecDeque<Arc<TaskControlBlock>>,
}

pub struct SemaphoreGuard<'a, T> {
    wait_queue: Arc<WaitQueue>,
    count: &'a UnsafeCell<isize>,
    data: *mut T,
}

impl <T> Drop for SemaphoreGuard<'_, T> {
    fn drop(&mut self) {
        // 释放锁
        unsafe { *self.count.get() += 1; }
        if unsafe{*self.count.get() <= 0} {
            // 唤醒等待队列中的一个线程
            wake_up_one(self.wait_queue.clone());
        }
        
    }
}

impl <T> Deref for SemaphoreGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}

impl <T> DerefMut for SemaphoreGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.data }
    }
}