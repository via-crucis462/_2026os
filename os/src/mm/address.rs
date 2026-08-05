//! 地址转换相关实现
//! 这里的地址/页号大部分方法按照标准页定义，需要注意
//! 实际上很多地方用大页，因此调用时需要注意大页和标准页的关系

use super::pte::PageTableEntry;
use crate::arch::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use core::fmt::{self, Debug, Formatter};

use crate::arch::mm::*;
const PPN_WIDTH: usize = PA_WIDTH - PAGE_SIZE_BITS;
const VPN_WIDTH: usize = VA_WIDTH - PAGE_SIZE_BITS;

#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
/// 物理地址
/// 
/// 物理地址一定是无符号的
/// 
/// 规定无论是 rv 还是 la，这里的物理地址都存放真正的“物理”地址，而不是内核虚拟地址
/// 
/// 当需要用裸 usize 值时，特别规定：
/// pa.0 代表物理地址的实际值
/// --- 现行实现下 riscv64 无是否带缓存的区分，但内核仍需用窗口虚拟地址访存 ---
/// pa.get_cached_addr() 代表带缓存窗口映射值（内核用指针访存应一律使用该值）
/// pa.get_uncached_addr() 代表不可缓存窗口映射值
/// 
pub struct PhysAddr(pub usize);

/// 虚拟地址
/// 
/// 虚拟地址为有符号值，可以理解为 VA_WIDTH 长的补码
/// 
/// 规定其 .0 值为 39 位有符号值（即忽略高位0），转为 usize 时符号扩展为 64 位
/// 
/// 特别规定：
/// 内核函数在不需要保持和用户态虚拟地址对齐时，可以使用位运算计算地址来使语义更清晰，
/// 这里的虚拟地址类型主要用于访问和管理用户页表
/// 
#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
///virtual address
pub struct VirtAddr(pub usize);

/// 按标准页计算的物理页号
/// 
/// 物理页号一定是无符号的
/// 
#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct PhysPageNum(pub usize);

/// 按标准页计算的虚拟页号
///
/// 虚拟页号本质上是三级页表的索引值，所以是无符号值，这点与 VA 不同
/// 
#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct VirtPageNum(pub usize);

/// Debugging

impl Debug for VirtAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("VA:0x{:x}", self.0))
    }
}
impl Debug for VirtPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("VPN:0x{:x}", self.0))
    }
}
impl Debug for PhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("PA:0x{:x}", self.0))
    }
}
impl Debug for PhysPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("PPN:0x{:x}", self.0))
    }
}

/// T: {PhysAddr, VirtAddr, PhysPageNum, VirtPageNum}
/// T -> usize: T.0
/// usize -> T: usize.into()

impl From<usize> for PhysAddr {
    fn from(v: usize) -> Self {
        Self(v & ((1 << PA_WIDTH) - 1))
    }
}
impl From<usize> for PhysPageNum {
    fn from(v: usize) -> Self {
        Self(v & ((1 << PPN_WIDTH) - 1))
    }
}
impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        Self(v & ((1 << VA_WIDTH) - 1))
    }
}

impl From<usize> for VirtPageNum {
    /// 注意：这里的输入为虚拟页号值，不是地址
    fn from(v: usize) -> Self {
        Self(v & ((1 << VPN_WIDTH) - 1))
    }
}
impl From<PhysPageNum> for usize {
    fn from(v: PhysPageNum) -> Self {
        v.0
    }
}

impl From<VirtAddr> for usize {
    fn from(v: VirtAddr) -> Self {
        if v.0 >= (1 << (VA_WIDTH - 1)) {
            v.0 | (!((1 << VA_WIDTH) - 1))
        } else {
            v.0
        }
    }
}
// 页号是无符号索引，不考虑高半低半地址空间
impl From<VirtPageNum> for usize {
    fn from(v: VirtPageNum) -> Self {
        v.0
    }
}

impl VirtAddr {
    /// 还原完整 64 位规范地址（把 [VA_WIDTH-1:0] 符号扩展）
    /// 
    /// 等价于 usize::from(*self)
    pub fn full_addr(&self) -> usize {
        usize::from(*self)
    }

    /// Get the (floor) virtual page number
    pub fn std_floor(&self) -> VirtPageNum {
        VirtPageNum(self.0 / PAGE_SIZE)
    }

    /// Get the (ceil) virtual page number
    pub fn std_ceil(&self) -> VirtPageNum {
        VirtPageNum((self.0 - 1 + PAGE_SIZE) / PAGE_SIZE)
    }

    /// Get the page offset of virtual address
    /// 当作4k标准页的偏移量
    pub fn std_page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    /// 按照实际页大小计算偏移量，适用于大页
    pub fn actual_page_offset(&self, page_size: super::PageSize) -> usize {
        self.0 & (page_size.size() - 1)
    }
    /// Check if the virtual address is aligned by page size
    pub fn std_aligned(&self) -> bool {
        self.std_page_offset() == 0
    }
    pub fn actual_aligned(&self, page_size: super::PageSize) -> bool {
        self.actual_page_offset(page_size) == 0
    }
}
impl From<VirtAddr> for VirtPageNum {
    fn from(v: VirtAddr) -> Self {
        assert_eq!(v.std_page_offset(), 0);
        v.std_floor()
    }
}
impl From<VirtPageNum> for VirtAddr {
    fn from(v: VirtPageNum) -> Self {
        Self(v.0 << PAGE_SIZE_BITS)
    }
}

impl PhysAddr {
    /// Get the immutable reference of physical address
    /// 用 cached 地址，不允许在访问硬件 mmio 等情况下使用
    pub fn get_ref<T>(&self) -> &'static T {
        unsafe { (self.get_cached_addr() as *const T).as_ref().unwrap() }
    }
    /// Get the mutable reference of physical address
    /// 用 cached 地址，不允许在访问硬件mmio 等情况下使用
    pub fn get_mut<T>(&self) -> &'static mut T {
        unsafe { (self.get_cached_addr() as *mut T).as_mut().unwrap() }
    }
    /// Get the (floor) physical page number
    /// 向下取整
    pub fn std_floor(&self) -> PhysPageNum {
        PhysPageNum(self.0 / PAGE_SIZE)
    }
    /// Get the (ceil) physical page number
    /// 向上取整
    pub fn std_ceil(&self) -> PhysPageNum {
        PhysPageNum((self.0  + PAGE_SIZE - 1) / PAGE_SIZE)
    }
    /// Get the page offset of physical address
    pub fn std_page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    /// 按照实际页大小计算偏移量，适用于大页
    pub fn actual_page_offset(&self, page_size: super::PageSize) -> usize {
        self.0 & (page_size.size() - 1)
    }
    /// Check if the physical address is aligned by page size
    pub fn std_aligned(&self) -> bool {
        self.std_page_offset() == 0
    }
    /// 考虑大页的对齐检查
    pub fn actual_aligned(&self, page_size: super::PageSize) -> bool {
        self.actual_page_offset(page_size) == 0
    }
    /// 获取可缓存窗口映射后的内核态地址值
    /// 
    /// 规定为内核自身内存访问使用
    /// 
    /// riscv64 不区分可缓存和不可缓存的窗口映射
    pub fn get_cached_addr(&self) -> usize {
        self.0 | crate::CACHED_KERNEL_BASE
    }
    /// 获取不可缓存窗口映射后的内核态地址值
    ///
    /// 规定为设备访问时使用
    /// 
    /// riscv64 不区分可缓存和不可缓存的窗口映射
    pub fn get_uncached_addr(&self) -> usize {
        self.0 | crate::UNCACHED_KERNEL_BASE
    }
}

impl From<PhysAddr> for PhysPageNum {
    fn from(v: PhysAddr) -> Self {
        assert_eq!(v.std_page_offset(), 0);
        v.std_floor()
    }
}
impl From<PhysPageNum> for PhysAddr {
    fn from(v: PhysPageNum) -> Self {
        Self(v.0 << PAGE_SIZE_BITS)
    }
}

impl VirtPageNum {
    /// 获取起始虚拟地址值（返回 64 位有符号值）
    /// 等价于 usize::from(VirtAddr::from(*self))
    pub fn start_addr(&self) -> usize {
        usize::from(VirtAddr::from(*self))
    }

    /// Get the indexes of the page table entry
    /// la64的页表索引顺序与SV39相同
    pub fn indexes(&self) -> [usize; 3] {
        let mut vpn = self.0;
        let mut idx = [0usize; 3];
        for i in (0..3).rev()/*翻转*/ {
            idx[i] = vpn & ((PAGE_SIZE>>3) - 1);
            vpn >>= PAGE_SIZE_BITS - 3;
        }
        idx
    }
}

impl PhysPageNum {
    /// Get the reference of page table(array of ptes)
    /// pte 一定是按标准页组织的，因为一个9位页号对应页表占用一个标准页
    pub fn get_pte_array(&self) -> &'static mut [PageTableEntry] {
        let pa: PhysAddr = (*self).into();
        unsafe { core::slice::from_raw_parts_mut(pa.get_cached_addr() as *mut PageTableEntry, PAGE_SIZE>>3) }
    }
    /// Get the reference of page(array of bytes)
    /// 以基本页为单位，返回该页页的字节数组
    pub fn get_bytes_array(&self) -> &'static mut [u8] {
        let pa: PhysAddr = (*self).into();
        unsafe { core::slice::from_raw_parts_mut(pa.get_cached_addr() as *mut u8, PAGE_SIZE) }
    }
    /// 支持大页的版本
    pub fn get_bytes_array_with_size(&self, page_size: super::PageSize) -> &'static mut [u8] {
        let pa: PhysAddr = (*self).into();
        assert!(pa.actual_aligned(page_size), "physical address 0x{:x} is not aligned by page size {}!", pa.0, page_size.size());
        unsafe { core::slice::from_raw_parts_mut(pa.get_cached_addr() as *mut u8, page_size.size()) }
    }
    /// Get the mutable reference of physical address
    pub fn get_mut<T>(&self) -> &'static mut T {
        let pa: PhysAddr = (*self).into();
        pa.get_mut()
    }
}

/// iterator for phy/virt page number
pub trait StepByOne {
    /// step by one element(page number)
    fn step(&mut self);
    /// 按实际步长走，这里的步长指的是实际页大小对应标准页大小的倍数
    fn step_by(&mut self, steps: usize) {
        for _ in 0..steps {
            self.step();
        }
    }
}
impl StepByOne for VirtPageNum {
    fn step(&mut self) {
        self.0 += 1;
    }
    fn step_by(&mut self, steps: usize) {
        self.0 += steps;
    }
}
impl StepByOne for PhysPageNum {
    fn step(&mut self) {
        self.0 += 1;
    }
    fn step_by(&mut self, steps: usize) {
        self.0 += steps;
    }
}

#[derive(Copy, Clone)]
/// a simple range structure for type T
/// 只能对标准页大小使用，如果使用vpn表示一个大页，则需要注意范围的定义和迭代时的步长
/// 因此不能轻易使用simplerage的迭代器
pub struct SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    l: T,
    r: T,
}
impl<T> SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(start: T, end: T) -> Self {
        assert!(start <= end, "start {:?} > end {:?}!", start, end);
        Self { l: start, r: end }
    }
    /// 生成一个带步长的range，适用于可变页大小的情况
    pub fn add_step(&mut self, step: usize) -> RangeWithStep<T> {
        RangeWithStep::new(self.l, self.r, step)
    }
    pub fn get_start(&self) -> T {
        self.l
    }
    pub fn get_end(&self) -> T {
        self.r
    }
    pub fn contains(&self, t: T) -> bool {
        self.l <= t && t < self.r
    }
}
impl<T> IntoIterator for SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    type IntoIter = SimpleRangeIterator<T>;
    fn into_iter(self) -> Self::IntoIter {
        SimpleRangeIterator::new(self.l, self.r)
    }
}
/// iterator for the simple range structure
pub struct SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    current: T,
    end: T,
}
impl<T> SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(l: T, r: T) -> Self {
        Self { current: l, end: r }
    }
}
impl<T> Iterator for SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.end {
            None
        } else {
            let t = self.current;
            self.current.step();
            Some(t)
        }
    }
}
/// a simple range structure for virtual page number
/// 标准页范围
pub type VPNRange = SimpleRange<VirtPageNum>;

/// 带步长的range，先写在这备用，后续考虑抽象一下地址翻译的循环设计，
/// 现在的实现是每一处都写一次循环，跟在写c一样，不过反正能跑，懒的改了:(
#[derive(Copy, Clone)]
pub struct RangeWithStep<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    l: T,
    r: T,
    step: usize,
}
impl<T> RangeWithStep<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(start: T, end: T, step: usize) -> Self {
        assert!(start <= end, "start {:?} > end {:?}!", start, end);
        Self { l: start, r: end, step }
    }
    pub fn get_start(&self) -> T {
        self.l
    }
    pub fn get_end(&self) -> T {
        self.r
    }
}
impl<T> IntoIterator for RangeWithStep<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    type IntoIter = StepRangeIterator<T>;
    fn into_iter(self) -> Self::IntoIter {
        StepRangeIterator::new(self.l, self.r, self.step)
    }
}
pub struct StepRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    current: T,
    end: T,
    step: usize,
}
impl<T> StepRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(l: T, r: T, step: usize) -> Self {
        Self { current: l, end: r, step }
    }
}
impl<T> Iterator for StepRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.end {
            None
        } else {
            let t = self.current;
            self.current.step_by(self.step);
            Some(t)
        }
    }
}
/// a simple range structure for virtual page number
/// 页范围
pub type VPNRangeWithStep = RangeWithStep<VirtPageNum>;