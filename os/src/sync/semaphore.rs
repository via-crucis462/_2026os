//! 信号量互斥锁实现
use core::cell::UnsafeCell;
use core::ops::Deref;
use core::panic;

use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use crate::process::*;
use core::marker::PhantomData;
use core::ops::DerefMut;

use core::sync::atomic::{AtomicIsize, Ordering};

use spin::Mutex;


pub struct Semaphore<T> {
    count: AtomicIsize,// 原子化isize
    wait_queue: Mutex<WaitQueue>,// 自旋锁，多核下保护等待队列
    inner: UnsafeCell<T>,
}

impl<T> Semaphore<T> {
    pub fn new(count: isize, data: T) -> Self {
        Self {
            count: AtomicIsize::new(count),
            wait_queue: Mutex::new(WaitQueue { queue: VecDeque::new() }),
            inner: UnsafeCell::new(data),
        }
    }
    pub fn lock(&self) -> SemaphoreGuard<'_, T>{

        if self.count.fetch_sub(1, Ordering::Acquire)/*返回的是旧值*/ < 1 {
            let queue = self.wait_queue.lock();
            // 当前线程进入等待队列
            current_task_to_sleep(queue);
        }
        SemaphoreGuard {
            sem: self,
        }
    }
}

pub struct WaitQueue {
    queue: VecDeque<Arc<TaskControlBlock>>,
}

impl WaitQueue {
    pub fn push_back(&mut self, task: Arc<TaskControlBlock>) {
        self.queue.push_back(task);
    }
    pub fn pop_front(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.queue.pop_front()
    }
}

pub struct SemaphoreGuard<'a, T> {
    sem: &'a Semaphore<T>,
}

impl <T> Drop for SemaphoreGuard<'_, T> {
    fn drop(&mut self) {
        // 释放锁
        // 上面的代码必须执行完才能执行这个
        if self.sem.count.fetch_add(1, Ordering::Release)/*返回的是旧值*/ < 0 {
            let queue = self.sem.wait_queue.lock();
            // 唤醒等待队列中的一个线程
            wake_up_one(queue);
        }
    }
}

impl <T> Deref for SemaphoreGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.sem.inner.get() }
    }
}

impl <T> DerefMut for SemaphoreGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.sem.inner.get() }
    }
}