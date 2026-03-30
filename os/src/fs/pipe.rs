use super::File;
use crate::mm::UserBuffer;
use crate::sync::MPSafeCell;
use alloc::sync::{Arc, Weak};
use crate::mm::{frame_alloc, FrameTracker}; 
use crate::arch::config::PAGE_SIZE;

use crate::task::suspend_current_and_run_next;

/// IPC pipe
pub struct Pipe {
    readable: bool,
    writable: bool,
    buffer: Arc<MPSafeCell<PipeRingBuffer>>,
}

impl Pipe {
    /// create readable pipe
    pub fn read_end_with_buffer(buffer: Arc<MPSafeCell<PipeRingBuffer>>) -> Self {
        Self {
            readable: true,
            writable: false,
            buffer,
        }
    }
    /// create writable pipe
    pub fn write_end_with_buffer(buffer: Arc<MPSafeCell<PipeRingBuffer>>) -> Self {
        Self {
            readable: false,
            writable: true,
            buffer,
        }
    }
}

const RING_BUFFER_SIZE: usize = PAGE_SIZE * 16;

#[derive(Copy, Clone, PartialEq)]
enum RingBufferStatus {
    Full,
    Empty,
    Normal,
}

pub struct PipeRingBuffer {
    frames: alloc::vec::Vec<FrameTracker>,
    head: usize,
    tail: usize,
    status: RingBufferStatus,
    write_end: Option<Weak<Pipe>>,
    read_end: Option<Weak<Pipe>>,
}

impl PipeRingBuffer {
    pub fn new() -> Self {
        let mut frames = alloc::vec::Vec::new();
       for _ in 0..16 {
            frames.push(frame_alloc().expect("Failed to alloc physical frame for pipe!"));
        }
        Self {
            frames,
            head: 0,
            tail: 0,
            status: RingBufferStatus::Empty,
            write_end: None,
            read_end: None,
        }
    }
    pub fn set_write_end(&mut self, write_end: &Arc<Pipe>) {
        self.write_end = Some(Arc::downgrade(write_end));
    }
    pub fn set_read_end(&mut self, read_end: &Arc<Pipe>) {
        self.read_end = Some(Arc::downgrade(read_end));
    }
    pub fn write_byte(&mut self, byte: u8) {
        self.status = RingBufferStatus::Normal;
        
        // 找到物理页中的真实位置，写入数据
        *self.get_byte_mut(self.tail) = byte;
        
        self.tail = (self.tail + 1) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }
    // 内部辅助方法：根据当前的 head 或 tail 索引，拿到物理页中对应字节的可变引用
    fn get_byte_mut(&mut self, index: usize) -> &mut u8 {
        let frame_idx = index / PAGE_SIZE; // 在第几个物理页（0 或 1）
        let offset = index % PAGE_SIZE;    // 页内偏移
        let ppn = self.frames[frame_idx].ppn;
        
        // 获取该物理页的全体字节数组，然后取对应偏移的字节
        &mut ppn.get_bytes_array()[offset]
    }
    pub fn read_byte(&mut self) -> u8 {
        self.status = RingBufferStatus::Normal;
        
        // 从物理页中读取数据
        let c = *self.get_byte_mut(self.head);
        
        self.head = (self.head + 1) % RING_BUFFER_SIZE;
        
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;
        }
        c
    }
    
    pub fn available_read(&self) -> usize {
        if self.status == RingBufferStatus::Empty {
            0
        } else if self.tail > self.head {
            self.tail - self.head
        } else {
            self.tail + RING_BUFFER_SIZE - self.head
        }
    }
    pub fn available_write(&self) -> usize {
        if self.status == RingBufferStatus::Full {
            0
        } else {
            RING_BUFFER_SIZE - self.available_read()
        }
    }
    pub fn all_write_ends_closed(&self) -> bool {
        self.write_end.as_ref().unwrap().upgrade().is_none()
    }
    pub fn all_read_ends_closed(&self) -> bool {
        self.read_end.as_ref().unwrap().upgrade().is_none()
    }
}

/// Return (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(MPSafeCell::new(PipeRingBuffer::new()));
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.exclusive_access().set_write_end(&write_end);
    buffer.exclusive_access().set_read_end(&read_end);
    (read_end, write_end)
}

impl File for Pipe {
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }
    // 👇 加上重写的 ready_to_read
    fn ready_to_read(&self) -> bool {
        if !self.readable { return false; }
        let ring_buffer = self.buffer.exclusive_access();
        // 有数据可读，或者写端全关了（EOF），都算可读就绪
        ring_buffer.available_read() > 0 || ring_buffer.all_write_ends_closed()
    }

    // 👇 加上重写的 ready_to_write
    fn ready_to_write(&self) -> bool {
        if !self.writable { return false; }
        let ring_buffer = self.buffer.exclusive_access();
        let space = ring_buffer.available_write();
        if space == 0 {
            error!("[kernel] PIPE IS FULL! head={}, tail={}", ring_buffer.head, ring_buffer.tail);
        }
        // 有空间可写，或者读端全关了（BROKEN PIPE），都算可写就绪
        ring_buffer.available_write() > 0 || ring_buffer.all_read_ends_closed()
        
    }
    fn read(&self, buf: UserBuffer) -> usize {
        assert!(self.readable());
        let want_to_read = buf.len();
        let mut buf_iter = buf.into_iter();
        let mut already_read = 0usize;
        loop {
            let mut ring_buffer = self.buffer.exclusive_access();
            let loop_read = ring_buffer.available_read();
            if loop_read == 0 {
                if ring_buffer.all_write_ends_closed() {
                    return already_read;
                }
               //println!("[kernel] Pipe Read Empty: already_read={}, waiting...", already_read);
                drop(ring_buffer);
                suspend_current_and_run_next();
                continue;
            }
            for _ in 0..loop_read {
                if let Some(byte_ref) = buf_iter.next() {
                    unsafe {
                        *byte_ref = ring_buffer.read_byte();
                    }
                   
                    already_read += 1;
                    if already_read % 1024 == 0 {
                     //   println!("[kernel] Pipe Read Progress: {} / {}", already_read, want_to_read);
                    }
                    if already_read == want_to_read {
                        return want_to_read;
                    }
                } else {
                    return already_read;
                }
            }
        }
    }
    fn write(&self, buf: UserBuffer) -> usize {
        assert!(self.writable());
        let want_to_write = buf.len();
        let mut buf_iter = buf.into_iter();
        let mut already_write = 0usize;
        loop {
            let mut ring_buffer = self.buffer.exclusive_access();
            let loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                if ring_buffer.all_read_ends_closed() {
                    return already_write;
                }
              //  println!("[kernel] Pipe Write Full: already_write={}, waiting for consumer...", already_write);
                drop(ring_buffer);
                suspend_current_and_run_next();
                continue;
            }
            // write at most loop_write bytes
            for _ in 0..loop_write {
                if let Some(byte_ref) = buf_iter.next() {
                    if ring_buffer.all_read_ends_closed() {
                        return already_write;
                    }
                    ring_buffer.write_byte(unsafe { *byte_ref });
                    already_write += 1;
                    if already_write % 1024 == 0 {
                  //      println!("[kernel] Pipe Write Progress: {} / {}", already_write, want_to_write);
                    }
                    if already_write == want_to_write {
                        return want_to_write;
                    }
                } else {
                    return already_write;
                }
            }
        }
    }

    fn read_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.read(buf)
    }

    fn write_at(&self, _offset: usize, buf: UserBuffer) -> usize {
        self.write(buf)
    }

    fn get_stat(&self) -> super::Stat {
        super::Stat {
            mode: 0o010000,
            ..Default::default()
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize {
        trace!("Pipe: getdents called on a pipe, returning -1");
        -1
    }
}
