use alloc::vec::Vec;

/// An abstraction over a buffer passed from user space to kernel space
pub struct UserBuffer {
    /// A list of buffers
    pub buffers: Vec<&'static mut [u8]>,
}

impl UserBuffer {
    /// Constuct UserBuffer
    pub fn new(buffers: Vec<&'static mut [u8]>) -> Self {
        Self { buffers }
    }
    /// Get the length of the buffer
    pub fn len(&self) -> usize {
        let mut total: usize = 0;
        for b in self.buffers.iter() {
            total += b.len();
        }
        total
    }
    /// 将内核中的连续字节流（如结构体转化的 &[u8] 或文件缓存）写入用户态缓冲区
    pub fn write(&mut self, data: &[u8]) {
        let mut current = 0;
        for buffer in self.buffers.iter_mut() {
            let remain = data.len() - current;
            if remain == 0 {
                break;
            }
            // 取当前 buffer 容量和剩余待写数据长度的较小值，防止越界
            let copy_len = core::cmp::min(buffer.len(), remain);
            // 利用 memcpy 级别的高效拷贝
            buffer[..copy_len].copy_from_slice(&data[current..current + copy_len]);
            current += copy_len;
        }
    }
    /// 从用户态缓冲区读取数据，存入内核中的连续字节流中
    pub fn read(&self, data: &mut [u8]) {
        let mut current = 0;
        for buffer in self.buffers.iter() {
            let remain = data.len() - current;
            if remain == 0 {
                break;
            }
            let copy_len = core::cmp::min(buffer.len(), remain);
            data[current..current + copy_len].copy_from_slice(&buffer[..copy_len]);
            current += copy_len;
        }
    }
    /// buffer到buffer的版本，之前写东西实现的后来发现没必要，实际上很少用到
    pub fn read_into_buffer(&self, mut data: Self) -> isize {
        let mut i = 0;
        let mut j = 0;
        for buf in self.buffers.iter() {
            for ch in buf.iter() {
                if let Some(da) = data.buffers[i].get_mut(j) {
                    *da = unsafe {*ch};
                    i += 1;
                } else {
                    i = 0;
                    j += 1;
                    break;
                }
            }
        }
        data.len() as isize
    }
    /// buffer到buffer的版本
    pub fn write_from_buffer(&mut self, data: Self) -> isize {
        let mut i = 0;
        let mut j = 0;
        for buf in self.buffers.iter_mut() {
            for ch in buf.iter_mut() {
                if let Some(da) = data.buffers[i].get(j) {
                    *ch = unsafe {*da};
                    i += 1;
                } else {
                    i = 0;
                    j += 1;
                    break;
                }
            }
        }
        data.len() as isize
    }
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

/// An iterator over a UserBuffer
pub struct UserBufferIterator {
    buffers: Vec<&'static mut [u8]>,
    current_buffer: usize,
    current_idx: usize,
}

impl Iterator for UserBufferIterator {
    type Item = *mut u8;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current_buffer >= self.buffers.len() {
            None
        } else {
            let r = &mut self.buffers[self.current_buffer][self.current_idx] as *mut _;
            if self.current_idx + 1 == self.buffers[self.current_buffer].len() {
                self.current_idx = 0;
                self.current_buffer += 1;
            } else {
                self.current_idx += 1;
            }
            Some(r)
        }
    }
}
