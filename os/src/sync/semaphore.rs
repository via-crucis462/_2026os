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
            let queue = &self.wait_queue;
            // 当前线程进入等待队列
            block_current_and_run_next(queue);
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
    pub fn new() -> Self {
        Self { queue: VecDeque::new() }
    }
    pub fn push_back(&mut self, task: Arc<TaskControlBlock>) {
        self.queue.push_back(task);
    }
    pub fn pop_front(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.queue.pop_front()
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn remove_by_tid(&mut self, tid: usize) {
        self.queue.retain(|task| task.gettid() != tid);
        
    }
    pub fn front(&self) -> Option<Arc<TaskControlBlock>> {
        self.queue.front().cloned()
    }
    /// 获取当前队列中等待的任务数量
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// 获取队列中第一个（最先进入）任务的 TID（不弹出任务）
    pub fn front_tid(&self) -> Option<usize> {
        self.queue.front().map(|task| task.gettid())
    }

    /// 获取当前队列中所有任务的 TID 列表
    pub fn get_tids(&self) -> Vec<usize> {
        self.queue.iter().map(|task| task.gettid()).collect()
    }
    pub fn remove_task(&mut self, tid: usize) {
        self.queue.retain(|task| task.gettid() != tid);
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
            let queue = &self.sem.wait_queue;
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