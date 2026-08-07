//! 用户缓冲区相关的结构体和方法
//! 
//! 当前实现参考了 Linux 的 GUP 机制，缓冲区结构体会携带页帧的 Arc 所有权

use super::{FrameTracker, PhysAddr};
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use core::slice;

/// 用户空间缓冲区段
///
/// - 将用户地址翻译为缓冲区时，通过 frame 取 Some() 来固定普通用户页的所有权（保证访问期间
/// 在放掉所有 mm 锁后相应页帧不会被其他核的内存操作释放）
///
/// - 而部分内核内存（例如栈上数组和部分堆内存）则通过 frame 取 None 来表示不需要固定所有权，
/// 调用者需要自己保证 ptr 指向的内存不会在结构体的生命周期内被释放或被其他引用访问
pub struct UserBufferSegment {
    ptr: *mut u8,
    len: usize,
    frame: Option<FrameTracker>,
}

unsafe impl Send for UserBufferSegment {}

impl UserBufferSegment {
    pub(crate) fn from_frame(frame: FrameTracker, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > frame.page_size.size() {
            return None;
        }
        let ptr = PhysAddr::from(frame.ppn)
            .get_cached_addr()
            .checked_add(offset)? as *mut u8;
        Some(Self {
            ptr,
            len,
            frame: Some(frame),
        })
    }

    /// 从物理地址直接构建一个段
    ///
    /// 调用者需要保证 ptr 指向的内存不会在结构体的生命周期内被释放或被其他引用访问
    pub(crate) unsafe fn from_untracked_phys(
        ppn: super::PhysPageNum,
        page_size: super::PageSize,
        offset: usize,
        len: usize,
    ) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > page_size.size() {
            return None;
        }
        let ptr = PhysAddr::from(ppn).get_cached_addr().checked_add(offset)? as *mut u8;
        Some(Self {
            ptr,
            len,
            frame: None,
        })
    }

    /// 从切片构建一个段
    ///
    /// 调用者需要保证 ptr 指向的内存不会在结构体的生命周期内被释放或被其他引用访问
    pub unsafe fn from_kernel_slice(buffer: &mut [u8]) -> Self {
        Self {
            ptr: buffer.as_mut_ptr(),
            len: buffer.len(),
            frame: None,
        }
    }

    pub fn is_pinned(&self) -> bool {
        self.frame.is_some()
    }
}

impl Deref for UserBufferSegment {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl DerefMut for UserBufferSegment {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

/// An abstraction over a buffer passed between user and kernel space.
pub struct UserBuffer {
    pub buffers: Vec<UserBufferSegment>,
}

impl UserBuffer {
    pub fn new(buffers: Vec<UserBufferSegment>) -> Self {
        Self { buffers }
    }

    /// 从切片构建一个用户缓冲区
    ///
    /// 调用者需要保证 ptr 指向的内存不会在结构体的生命周期内被释放或被其他引用访问
    pub unsafe fn from_kernel_slice(buffer: &mut [u8]) -> Self {
        Self::new(alloc::vec![unsafe {
            UserBufferSegment::from_kernel_slice(buffer)
        }])
    }

    pub fn len(&self) -> usize {
        self.buffers.iter().map(|buffer| buffer.len()).sum()
    }

    pub fn write(&mut self, data: &[u8]) {
        let mut current = 0;
        for buffer in self.buffers.iter_mut() {
            let remain = data.len().saturating_sub(current);
            if remain == 0 {
                break;
            }
            let copy_len = core::cmp::min(buffer.len(), remain);
            buffer[..copy_len].copy_from_slice(&data[current..current + copy_len]);
            current += copy_len;
        }
    }

    pub fn read(&self, data: &mut [u8]) {
        let mut current = 0;
        for buffer in self.buffers.iter() {
            let remain = data.len().saturating_sub(current);
            if remain == 0 {
                break;
            }
            let copy_len = core::cmp::min(buffer.len(), remain);
            data[current..current + copy_len].copy_from_slice(&buffer[..copy_len]);
            current += copy_len;
        }
    }

    pub fn read_into_buffer(&self, mut data: Self) -> isize {
        copy_segments(&self.buffers, &mut data.buffers) as isize
    }

    pub fn write_from_buffer(&mut self, data: Self) -> isize {
        copy_segments(&data.buffers, &mut self.buffers) as isize
    }
}

// 将 source 中的内容拷贝到 destination 中，返回拷贝的字节数
fn copy_segments(source: &[UserBufferSegment], destination: &mut [UserBufferSegment]) -> usize {
    let (mut source_index, mut source_offset) = (0, 0);
    let (mut destination_index, mut destination_offset) = (0, 0);
    let mut copied = 0;

    while source_index < source.len() && destination_index < destination.len() {
        let source_remaining = source[source_index].len() - source_offset;
        let destination_remaining = destination[destination_index].len() - destination_offset;
        let count = core::cmp::min(source_remaining, destination_remaining);

        unsafe {
            core::ptr::copy(
                source[source_index].ptr.add(source_offset),
                destination[destination_index].ptr.add(destination_offset),
                count,
            );
        }
        copied += count;
        source_offset += count;
        destination_offset += count;

        if source_offset == source[source_index].len() {
            source_index += 1;
            source_offset = 0;
        }
        if destination_offset == destination[destination_index].len() {
            destination_index += 1;
            destination_offset = 0;
        }
    }

    copied
}

impl IntoIterator for UserBuffer {
    type Item = *mut u8;
    type IntoIter = UserBufferIterator;

    fn into_iter(self) -> Self::IntoIter {
        UserBufferIterator {
            buffers: self.buffers,
            current_buffer: 0,
            current_idx: 0,
        }
    }
}

pub struct UserBufferIterator {
    buffers: Vec<UserBufferSegment>,
    current_buffer: usize,
    current_idx: usize,
}

impl Iterator for UserBufferIterator {
    type Item = *mut u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_buffer >= self.buffers.len() {
            return None;
        }
        let result = unsafe { self.buffers[self.current_buffer].ptr.add(self.current_idx) };
        self.current_idx += 1;
        if self.current_idx == self.buffers[self.current_buffer].len {
            self.current_idx = 0;
            self.current_buffer += 1;
        }
        Some(result)
    }
}
