use alloc::{
    collections::BTreeMap,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    mem::size_of,
};
use spin::Mutex;

use crate::{
    auth::{FileMode, PermStat},
    fs::{File, Stat},
    mm::{translated_read, translated_write},
    task::current_task,
};

use super::errno::Errno;

const BPF_MAP_CREATE: usize = 0;
const BPF_MAP_LOOKUP_ELEM: usize = 1;
const BPF_MAP_UPDATE_ELEM: usize = 2;
const BPF_PROG_LOAD: usize = 5;

const BPF_MAP_TYPE_HASH: u32 = 1;
const BPF_MAP_TYPE_ARRAY: u32 = 2;
const BPF_PROG_TYPE_SOCKET_FILTER: u32 = 1;
const BPF_PSEUDO_MAP_FD: u8 = 1;
const BPF_ANY: u64 = 0;
const BPF_NOEXIST: u64 = 1;
const BPF_EXIST: u64 = 2;

const BPF_LD_IMM_DW: u8 = 0x18;
const BPF_MOV64_IMM: u8 = 0xb7;
const BPF_MOV64_REG: u8 = 0xbf;
const BPF_ALU64_ADD_IMM: u8 = 0x07;
const BPF_ALU64_SUB_IMM: u8 = 0x17;
const BPF_ST_MEM_W: u8 = 0x62;
const BPF_ST_MEM_DW: u8 = 0x7a;
const BPF_STX_MEM_DW: u8 = 0x7b;
const BPF_JMP_JEQ_IMM: u8 = 0x15;
const BPF_JMP_JNE_IMM: u8 = 0x55;
const BPF_JMP_CALL: u8 = 0x85;
const BPF_EXIT: u8 = 0x95;
const BPF_FUNC_MAP_LOOKUP_ELEM: i32 = 1;

const BPF_STACK_SIZE: usize = 512;
const MAP_VALUE_HANDLE_BASE: u64 = 1 << 63;

#[repr(C)]
#[derive(Clone, Copy)]
struct BpfInsn {
    code: u8,
    regs: u8,
    off: i16,
    imm: i32,
}

impl BpfInsn {
    fn dst_reg(self) -> usize {
        (self.regs & 0x0f) as usize
    }

    fn src_reg(self) -> usize {
        (self.regs >> 4) as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BpfMapCreateAttr {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
}

//查询结构体BpfMapElemAttr包含了map_fd（BPF map的文件描述符）、key（要查询的键的地址）、value（存储查询结果的地址）和flags（查询选项）
#[repr(C)]
#[derive(Clone, Copy)]
struct BpfMapElemAttr {
    map_fd: u32,
    _pad0: u32,
    key: u64,
    value: u64,
    flags: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BpfProgLoadAttr {
    prog_type: u32,
    insn_cnt: u32,
    insns: u64,
    license: u64,
    log_level: u32,
    log_size: u32,
    log_buf: u64,
    kern_version: u32,
    prog_flags: u32,
}

enum BpfMapStorage {
    Array(Vec<Vec<u8>>),//数组存储每个键对应的值，键是索引，值是字节数组
    Hash(BTreeMap<Vec<u8>, Vec<u8>>),//哈希表存储键值对，键和值都是字节数组
}

pub struct BpfMapFile {
    map_type: u32,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
    storage: Mutex<BpfMapStorage>,//BpfMap和外部交互的共享存储区
}

impl BpfMapFile {
    fn new(map_type: u32, key_size: usize, value_size: usize, max_entries: usize) -> Result<Self, Errno> {
        if key_size == 0 || value_size == 0 || max_entries == 0 {
            return Err(Errno::EINVAL);
        }
        let storage = match map_type {
            BPF_MAP_TYPE_ARRAY => {
                let mut entries = Vec::with_capacity(max_entries);
                for _ in 0..max_entries {
                    entries.push(vec![0; value_size]);
                }
                BpfMapStorage::Array(entries)
            }
            BPF_MAP_TYPE_HASH => BpfMapStorage::Hash(BTreeMap::new()),
            _ => return Err(Errno::EINVAL),
        };
        Ok(Self {
            map_type,
            key_size,
            value_size,
            max_entries,
            storage: Mutex::new(storage),
        })
    }

    fn lookup_value(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Errno> {
        if key.len() != self.key_size {
            return Err(Errno::EINVAL);
        }
        let storage = self.storage.lock();
        match &*storage {
            BpfMapStorage::Array(entries) => {
                let index = self.array_index(key)?;
                Ok(entries.get(index).cloned())
            }
            BpfMapStorage::Hash(entries) => Ok(entries.get(key).cloned()),
        }
    }

    fn update_value(&self, key: &[u8], value: &[u8], flags: u64) -> Result<(), Errno> {
        if key.len() != self.key_size || value.len() != self.value_size {
            return Err(Errno::EINVAL);
        }
        let mut storage = self.storage.lock();
        match &mut *storage {
            BpfMapStorage::Array(entries) => {
                let index = self.array_index(key)?;
                let Some(slot) = entries.get_mut(index) else {
                    return Err(Errno::EINVAL);
                };
                slot.copy_from_slice(value);
                Ok(())
            }
            BpfMapStorage::Hash(entries) => {
                let exists = entries.contains_key(key);
                match flags {
                    BPF_NOEXIST if exists => return Err(Errno::EEXIST),
                    BPF_EXIST if !exists => return Err(Errno::ENOENT),
                    BPF_ANY | BPF_NOEXIST | BPF_EXIST => {}
                    _ => return Err(Errno::EINVAL),
                }
                if !exists && entries.len() >= self.max_entries {
                    return Err(Errno::ENOSPC);
                }
                entries.insert(key.to_vec(), value.to_vec());
                Ok(())
            }
        }
    }

    fn array_index(&self, key: &[u8]) -> Result<usize, Errno> {
        if self.map_type != BPF_MAP_TYPE_ARRAY || key.len() != 4 {
            return Err(Errno::EINVAL);
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize;
        if index >= self.max_entries {
            return Err(Errno::ENOENT);
        }
        Ok(index)
    }
}

impl File for BpfMapFile {
    fn readable(&self) -> bool { false }
    fn writable(&self) -> bool { false }

    fn get_perm(&self) -> PermStat {
        let mode = FileMode::from_bits_truncate(0o600);
        PermStat { mode, uid: 0, gid: 0 }
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 0,
            mode: 0o100000 | 0o600,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: 0,
            blksize: 0,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2],
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}

pub struct BpfProgFile {
    prog_type: u32,
    insns: Vec<BpfInsn>,
}

impl BpfProgFile {
    fn new(prog_type: u32, insns: Vec<BpfInsn>) -> Result<Self, Errno> {
        if prog_type != BPF_PROG_TYPE_SOCKET_FILTER || insns.is_empty() {
            return Err(Errno::EINVAL);
        }
        let prog = Self { prog_type, insns };
        prog.validate()?;
        Ok(prog)
    }

    fn validate(&self) -> Result<(), Errno> {
        let mut pc = 0usize;
        while pc < self.insns.len() {
            let insn = self.insns[pc];
            match insn.code {
                BPF_LD_IMM_DW => {
                    if pc + 1 >= self.insns.len() || insn.src_reg() as u8 != BPF_PSEUDO_MAP_FD {
                        return Err(Errno::EINVAL);
                    }
                    pc += 2;
                }
                BPF_MOV64_IMM
                | BPF_MOV64_REG
                | BPF_ALU64_ADD_IMM
                | BPF_ALU64_SUB_IMM
                | BPF_ST_MEM_W
                | BPF_ST_MEM_DW
                | BPF_STX_MEM_DW
                | BPF_JMP_JEQ_IMM
                | BPF_JMP_JNE_IMM
                | BPF_EXIT => {
                    pc += 1;
                }
                BPF_JMP_CALL => {
                    if insn.imm != BPF_FUNC_MAP_LOOKUP_ELEM {
                        return Err(Errno::EINVAL);
                    }
                    pc += 1;
                }
                _ => return Err(Errno::EINVAL),
            }
        }
        Ok(())
    }

    fn run(&self) -> Result<isize, Errno> {
        let mut regs = [0u64; 11];
        let mut stack = [0u8; BPF_STACK_SIZE];
        let mut handles: Vec<(usize, Vec<u8>)> = Vec::new();
        regs[10] = BPF_STACK_SIZE as u64;

        let mut pc = 0isize;
        while (pc as usize) < self.insns.len() {
            let insn = self.insns[pc as usize];
            let dst = insn.dst_reg();
            let src = insn.src_reg();
            match insn.code {
                BPF_LD_IMM_DW => {
                    let next = self.insns[(pc + 1) as usize];
                    let low = insn.imm as u32 as u64;
                    let high = next.imm as u32 as u64;
                    regs[dst] = low | (high << 32);
                    pc += 2;
                }
                BPF_MOV64_IMM => {
                    regs[dst] = insn.imm as i64 as u64;
                    pc += 1;
                }
                BPF_MOV64_REG => {
                    regs[dst] = regs[src];
                    pc += 1;
                }
                BPF_ALU64_ADD_IMM => {
                    regs[dst] = regs[dst].wrapping_add(insn.imm as i64 as u64);
                    pc += 1;
                }
                BPF_ALU64_SUB_IMM => {
                    regs[dst] = regs[dst].wrapping_sub(insn.imm as i64 as u64);
                    pc += 1;
                }
                BPF_ST_MEM_W => {
                    store_mem_imm(&mut stack, &mut handles, regs[dst], insn.off, 4, insn.imm as i64 as u64)?;
                    pc += 1;
                }
                BPF_ST_MEM_DW => {
                    store_mem_imm(&mut stack, &mut handles, regs[dst], insn.off, 8, insn.imm as i64 as u64)?;
                    pc += 1;
                }
                BPF_STX_MEM_DW => {
                    store_mem_value(&mut stack, &mut handles, regs[dst], insn.off, 8, regs[src])?;
                    pc += 1;
                }
                BPF_JMP_CALL => {
                    let map_fd = regs[1] as usize;
                    let map = get_bpf_map(map_fd)?;
                    let key = read_stack_range(&stack, regs[2], map.key_size)?;
                    regs[0] = if map.lookup_value(&key)?.is_some() {
                        let handle_id = handles.len();
                        handles.push((map_fd, key));
                        MAP_VALUE_HANDLE_BASE | handle_id as u64
                    } else {
                        0
                    };
                    pc += 1;
                }
                BPF_JMP_JEQ_IMM => {
                    if regs[dst] == insn.imm as i64 as u64 {
                        pc += insn.off as isize + 1;
                    } else {
                        pc += 1;
                    }
                }
                BPF_JMP_JNE_IMM => {
                    if regs[dst] != insn.imm as i64 as u64 {
                        pc += insn.off as isize + 1;
                    } else {
                        pc += 1;
                    }
                }
                BPF_EXIT => return Ok(regs[0] as isize),
                _ => return Err(Errno::EINVAL),
            }
        }
        Err(Errno::EINVAL)
    }
}

impl File for BpfProgFile {
    fn readable(&self) -> bool { false }
    fn writable(&self) -> bool { false }

    fn get_perm(&self) -> PermStat {
        let mode = FileMode::from_bits_truncate(0o600);
        PermStat { mode, uid: 0, gid: 0 }
    }

    fn get_stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: 0,
            mode: 0o100000 | 0o600,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            __pad: 0,
            size: self.insns.len() as i64,
            blksize: 0,
            __pad2: 0,
            blocks: 0,
            atime_sec: 0,
            atime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            ctime_sec: 0,
            ctime_nsec: 0,
            __unused: [0; 2],
        }
    }

    fn getdents(&self, _buf: &mut [u8]) -> isize { -1 }

    fn as_any(&self) -> &dyn Any { self }
}

//读取指定长度的用户空间数据到内核空间，并返回一个Vec<u8>，其中token是页表标识符，ptr是用户
fn read_user_bytes(token: usize, ptr: *const u8, len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    for offset in 0..len {
        data.push(translated_read(token, ptr.wrapping_add(offset)));
    }
    data
}
// 读取用户空间数据并写回用户空间，token是页表标识符，ptr是用户空间地址，data是要写入的数据
fn write_user_bytes(token: usize, ptr: *mut u8, data: &[u8]) {
    for (offset, byte) in data.iter().copied().enumerate() {
        translated_write(token, ptr.wrapping_add(offset), byte);
    }
}

fn set_log_buf(token: usize, ptr: u64, len: u32, message: &[u8]) {
    if ptr == 0 || len == 0 {
        return;
    }
    /*println!(
        "set_log_buf: ptr={:#x}, len={}, message_len={}",
        ptr,
        len,
        message.len()
    );*/
    let cap = len as usize;
    let message = message.strip_suffix(&[0]).unwrap_or(message);
    let copy_len = message.len().min(cap.saturating_sub(1));
    if copy_len > 0 {
        write_user_bytes(token, ptr as *mut u8, &message[..copy_len]);
    }
    translated_write(token, (ptr as *mut u8).wrapping_add(copy_len), 0u8);
}

fn install_bpf_fd(file: Arc<dyn File + Send + Sync>) -> Result<isize, Errno> {
    let task = current_task().unwrap();
    let process = task.process();
    let mut inner = process.inner_exclusive_access();
    let fd = inner.alloc_fd().ok_or(Errno::EMFILE)?;
    inner.set_fd(fd, file, false, 0);
    Ok(fd as isize)
}

fn get_bpf_map(fd: usize) -> Result<&'static BpfMapFile, Errno> {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return Err(Errno::EBADF);
    }
    let Some(file) = inner.fd_table[fd].file.as_ref() else {
        return Err(Errno::EBADF);
    };
    let map = file.as_any().downcast_ref::<BpfMapFile>().ok_or(Errno::EBADF)?;
    let map_ptr = map as *const BpfMapFile;
    drop(inner);
    Ok(unsafe { &*map_ptr })
}

fn get_bpf_prog(fd: usize) -> Result<&'static BpfProgFile, Errno> {
    let task = current_task().unwrap();
    let process = task.process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return Err(Errno::EBADF);
    }
    let Some(file) = inner.fd_table[fd].file.as_ref() else {
        return Err(Errno::EBADF);
    };
    let prog = file.as_any().downcast_ref::<BpfProgFile>().ok_or(Errno::EBADF)?;
    let prog_ptr = prog as *const BpfProgFile;
    drop(inner);
    Ok(unsafe { &*prog_ptr })
}

fn read_stack_range(stack: &[u8; BPF_STACK_SIZE], base: u64, len: usize) -> Result<Vec<u8>, Errno> {
    let start = base as isize;
    if start < 0 {
        return Err(Errno::EINVAL);
    }
    let start = start as usize;
    let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
    if end > stack.len() {
        return Err(Errno::EINVAL);
    }
    Ok(stack[start..end].to_vec())
}

fn store_mem_imm(
    stack: &mut [u8; BPF_STACK_SIZE],
    handles: &mut [(usize, Vec<u8>)],
    base: u64,
    off: i16,
    len: usize,
    value: u64,
) -> Result<(), Errno> {
    store_mem_value(stack, handles, base, off, len, value)
}

fn store_mem_value(
    stack: &mut [u8; BPF_STACK_SIZE],
    handles: &mut [(usize, Vec<u8>)],
    base: u64,
    off: i16,
    len: usize,
    value: u64,
) -> Result<(), Errno> {
    if base & MAP_VALUE_HANDLE_BASE != 0 {
        let handle = (base & !MAP_VALUE_HANDLE_BASE) as usize;
        let Some((map_fd, key)) = handles.get(handle) else {
            return Err(Errno::EINVAL);
        };
        let map = get_bpf_map(*map_fd)?;
        let mut entry = map.lookup_value(key)?.ok_or(Errno::ENOENT)?;
        let start = off as isize;
        if start < 0 {
            return Err(Errno::EINVAL);
        }
        let start = start as usize;
        let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
        if end > entry.len() {
            return Err(Errno::EINVAL);
        }
        let raw = value.to_ne_bytes();
        entry[start..end].copy_from_slice(&raw[..len]);
        return map.update_value(key, &entry, BPF_ANY);
    }

    let addr = (base as i64).wrapping_add(off as i64);
    if addr < 0 {
        return Err(Errno::EINVAL);
    }
    let start = addr as usize;
    let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
    if end > stack.len() {
        return Err(Errno::EINVAL);
    }
    let raw = value.to_ne_bytes();
    stack[start..end].copy_from_slice(&raw[..len]);
    Ok(())
}

fn bpf_map_create(token: usize, attr: *const u8, size: usize) -> isize {
    if size < size_of::<BpfMapCreateAttr>() {
        return Errno::EINVAL.as_isize();
    }
    let attr = translated_read(token, attr as *const BpfMapCreateAttr);
    match BpfMapFile::new(
        attr.map_type,
        attr.key_size as usize,
        attr.value_size as usize,
        attr.max_entries as usize,
    ) {
        Ok(map) => install_bpf_fd(Arc::new(map)).unwrap_or_else(|errno| errno.as_isize()),
        Err(errno) => errno.as_isize(),
    }
}

fn bpf_map_lookup(token: usize, attr: *const u8, size: usize) -> isize {
    if size < size_of::<BpfMapElemAttr>() {
        return Errno::EINVAL.as_isize();
    }
    let attr = translated_read(token, attr as *const BpfMapElemAttr);
    let Ok(map) = get_bpf_map(attr.map_fd as usize) else {
        return Errno::EBADF.as_isize();
    };
    let key = read_user_bytes(token, attr.key as *const u8, map.key_size);
    match map.lookup_value(&key) {
        Ok(Some(value)) => {
            write_user_bytes(token, attr.value as *mut u8, &value);
            0
        }
        Ok(None) => Errno::ENOENT.as_isize(),
        Err(errno) => errno.as_isize(),
    }
}

fn bpf_map_update(token: usize, attr: *const u8, size: usize) -> isize {
    if size < size_of::<BpfMapElemAttr>() {
        return Errno::EINVAL.as_isize();
    }
    let attr = translated_read(token, attr as *const BpfMapElemAttr);
    let Ok(map) = get_bpf_map(attr.map_fd as usize) else {
        return Errno::EBADF.as_isize();
    };
    let key = read_user_bytes(token, attr.key as *const u8, map.key_size);
    let value = read_user_bytes(token, attr.value as *const u8, map.value_size);
    map.update_value(&key, &value, attr.flags)
        .map(|_| 0)
        .unwrap_or_else(|errno| errno.as_isize())
}

fn bpf_prog_load(token: usize, attr: *const u8, size: usize) -> isize {
    //println!("bpf_prog_load called with attr={:#x}, size={}", attr as usize, size);
    if size < size_of::<BpfProgLoadAttr>() {
        return Errno::EINVAL.as_isize();
    }
    let attr_ptr = attr as usize;
    let attr = translated_read(token, attr as *const BpfProgLoadAttr);
    if attr.license == 0 {
        //println!("bpf_prog_load failed: license is required");
        set_log_buf(token, attr.log_buf, attr.log_size, b"missing license");
        return Errno::EINVAL.as_isize();
    }
    let mut insns = Vec::with_capacity(attr.insn_cnt as usize);
    for index in 0..attr.insn_cnt as usize {
        let insn_ptr = (attr.insns as *const BpfInsn).wrapping_add(index);
        insns.push(translated_read(token, insn_ptr));
    }
    match BpfProgFile::new(attr.prog_type, insns) {
        Ok(prog) => {
            set_log_buf(token, attr.log_buf, attr.log_size, b"\0");
            install_bpf_fd(Arc::new(prog)).unwrap_or_else(|errno| errno.as_isize())
        }
        Err(errno) => {
            set_log_buf(token, attr.log_buf, attr.log_size, b"unsupported bpf program");
            errno.as_isize()
        }
    }
}

pub fn sys_bpf(cmd: usize, attr: *const u8, size: usize) -> isize {
    let task = current_task().unwrap();
    let process = task.process();
    let token = process.inner_exclusive_access().memory_set.token();
    match cmd {
        BPF_MAP_CREATE => bpf_map_create(token, attr, size),    //创建一个BPF map并返回文件描述符，BPF是供用户空间程序与内核空间程序交互的一种机制，BPF map是BPF程序用来存储数据结构的对象
        BPF_MAP_LOOKUP_ELEM => bpf_map_lookup(token, attr, size),//在BPF map中查找元素
        BPF_MAP_UPDATE_ELEM => bpf_map_update(token, attr, size),//在BPF map中更新元素
        BPF_PROG_LOAD => bpf_prog_load(token, attr, size),//加载BPF程序
        _ => Errno::EINVAL.as_isize(),
    }
}

pub fn is_socket_filter_prog_fd(fd: usize) -> bool {
    matches!(get_bpf_prog(fd), Ok(prog) if prog.prog_type == BPF_PROG_TYPE_SOCKET_FILTER)
}

pub fn run_socket_filter_program(fd: usize) -> Result<isize, Errno> {
    let prog = get_bpf_prog(fd)?;
    prog.run()
}