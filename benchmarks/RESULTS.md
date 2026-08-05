# buildstorm 慢 17x 根因分析（2026-08-05）

同一负载（最终 ELF sha256 一致 `74e580...`）：内核 cargo 编译 6317s，Linux 358s。
内核侧 QEMU 累计 CPU 42521s（均值 7.08 核），Linux 约 1200s（均值约 3.3 核）。
结论：**不是 I/O 等待，也不是内存不足，而是并发 syscall 路径的全局锁/共享资源串行化 + 单线程 syscall 本身慢 2~3 倍**。

## 单线程 syscall 微基准（同一 QEMU TCG，static riscv64 ELF）

| 测试 | Linux | 内核 | 比值 |
|---|---:|---:|---:|
| getpid (1M) | 1391 ns | 2643 ns | 1.9x |
| clock_gettime (1M) | 183 ns | 4256 ns | 23x |
| open+close 命中 (100k) | 34.9 us | 34.9 us | 1.0x |
| open+close 未命中 (50k) | 18.5 us | 40.5 us | 2.2x |
| statx 命中 (100k) | 13.4 us | 31.1 us | 2.3x |
| readlinkat 未命中 (50k) | 14.6 us | 38.9 us | 2.7x |
| pread 4K (100k) | 5.8 us | 8.5 us | 1.5x |
| pread 64K (10k) | 11.9 us | 23.7 us | 2.0x |
| pwrite 4K (50k) | 12.6 us | 11.5 us | 1.0x |
| getdents /usr/bin (1k) | 1.55 ms | 0.67 ms | 0.4x |
| mmap+munmap 4K (50k) | 24.1 us | 24.0 us | 1.0x |
| mprotect 4K (50k) | 3.06 us | 6.72 us | 2.2x |
| 缺页 touch (16384) | 2.58 us | 7.51 us | 2.9x |
| futex ping-pong (100k) | 3.36 us | 3.07 us | 1.0x |
| fork+exec+wait (50) | 4.60 ms | 64.3 ms | 14x |

单线程只有 2~3x（exec 14x），不足以解释 17x。

## 8 路并发微基准（内核为宿主侧实测墙钟，Linux 为 guest 时钟，同一 TCG）

| 测试 | Linux 8 路 | 内核 8 路 | 内核单路 | 比值(vs Linux) |
|---|---:|---:|---:|---:|
| pure CPU | 0.60 s | 0.60 s | - | 1.0x |
| getpid | - | 4.26 s | 2.36 s | - |
| clock_gettime | - | 8.67 s | 4.38 s | - |
| mixed (open+statx+pread+close) | 1.51 s | 38.8 s | 1.25 s | 26x |
| pread 4K | 0.34 s | 35.0 s | 0.35 s | 102x |
| mmap+munmap 4K | 0.52 s | 33.3 s | 0.48 s | 64x |
| mprotect（8 进程） | 0.09 s | 0.22 s | 0.13 s | 2.5x |
| mprotect（8 线程共享 mm） | 0.43 s | 34.1 s | 0.13 s | 79x |
| open+close | 0.74 s | 3.05 s* | 0.58 s | 4.1x |

*open+close 是应用了路径分配优化后的结果；优化前同测例 8 路耗时约 10~90s（宿主实测），单路 0.59s。

## 关键结论（按证据强度排序）

1. **进程内多线程共享 mm 全局锁 + 每次 mprotect/mmap 的跨核 TLB shootdown（SBI remote_sfence_vma_asid）**：同一 mprotect 工作，8 进程 0.22s，8 线程（rustc 模型）34.1s，差 156 倍；Linux 8 线程只要 0.43s。buildstorm 主构建有 40 万次 mprotect，全部来自多线程 rustc。
2. **页缓存/块组全局锁 + 每页重复读 inode 元数据**：pread 8 路 35s（单路 0.35s，101 倍）。`find_physical_block` 每页都 `get_disk_inode`（块组互斥锁 + 元数据缓存全局锁），页缓存管理器每次访问锁 3~5 个全局 BTreeMap/LRU。
3. **全局内核堆锁**（`buddy_system_allocator::LockedHeap`）：每次 open/statx 的路径解析为每个分量分配 String + Vec，8 路并发时 open+close 退化到 10~90s；去掉路径分量逐项分配后降到 3.05s，直接证明堆锁争用是主要串行源之一。
4. **exec 整文件读入内核堆再逐段拷贝**：单线程 fork+exec+wait 64ms vs Linux 4.6ms（14x）；构建共 247 次 exec。
5. **无负 dentry 缓存**：readlinkat 66221 次中 65995 次失败，每次失败都走完整目录扫描；单线程已慢 2.7x。
6. 单线程基础 syscall 2~3x（clock_gettime 23x 最差，疑似 RTC/Goldfish 读法问题）。

## 已做的代码试验（保留在工作区，未提交）

- `os/src/fs/file_tree.rs`：
  - `find_child`：缓存命中不再持有父目录 `namespace_lock`，miss 时双检再取锁。
  - `find_tree`：初始路径分量改为借用切片（`VecDeque<Comp>`），符号链接展开才分配 String，去掉 `remove(0)` O(n) 搬移。
  - 效果：open+close 8 路从约 10~90s 降至 3.05s；单线程不变（0.58s）。

## 顺带发现的正确性/兼容性 bug

- `clock_gettime`/`gettimeofday` 在并发下返回错误/垃圾值（实测 nan、6x 高估、3x 低估交替出现），guest 侧计时工具不可信。
- 多进程并发写文件触发 ext4 extent 树断言 panic：`ext4inode.rs:402 non-leaf must have at least one index entry`（write 4 路必现）。
- 内核写 inode 不维护 ext4 metadata_csum：跑完 buildstorm 后镜像出现大量 inode 校验和错误，e2fsck 逐个中止；本次为修复镜像临时关闭了 metadata_csum 并 e2fsck。

## 建议修复优先级

1. mm 锁粒度：mprotect/mmap 页表更新与 TLB shootdown 批量/延迟化，避免每 syscall 一次 SBI remote fence；mprotect 热路径不要全量遍历 areas。
2. 页缓存：去掉每页 `get_disk_inode`/块组锁；`find_physical_block` 结果缓存；页缓存管理器改用 shard 或 RCU 风格读路径。
3. 内核堆：per-CPU 堆或分配缓存，消除全局 `LockedHeap` 争用（对 open/exec/mmap 都有效）。
4. exec 改为按段 mmap 页缓存 + 懒加载，不再 `read_all()` 到内核堆。
5. 负 dentry 缓存 + 目录项扫描读缓存优化，消灭 readlinkat/openat miss 的目录全扫描。

## 复现材料

- 源码：`benchmarks/sysbench.c`（单线程）、`benchmarks/sysbench_mp.c`（并发）
- 静态二进制：`benchmarks/build/sysbench*`（riscv64 static，可直接放进镜像执行）
- 原始日志：`benchmarks/*.log`（kernel-* 为内核侧，linux-* 为 Alpine 侧）
