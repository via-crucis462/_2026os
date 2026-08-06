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

## 2026-08-06 追加：纯 CPU O(n^3) 对照（同 QEMU/宿主，宿主时间戳）

同一份 `volatile` 累加三重循环，gcc -O2，几乎无系统调用：

| n | 内核耗时 | Alpine Linux 耗时 |
|---:|---:|---:|
| 200 | 0.052 s | 0.016 s |
| 400 | 0.105 s | 0.107 s |
| 600 | 0.358 s | 0.365 s |

单线程纯计算基本一致（~0.5 s 总量），说明数量级差距不在 CPU/TCG，
而在系统调用、内存管理、页缓存/块缓存等内核服务路径。
8 线程版本受宿主同时跑多个 QEMU 干扰，暂不作为结论。

### 2026-08-06 追加：停掉其他 QEMU 后的多线程纯计算（N=400，O(n^3)）

两台系统各自独占 QEMU 运行，同一程序、gcc -O2、pthread：

| 线程数 | 内核（两轮平均） | Alpine Linux（两轮平均） |
|---:|---:|---:|
| 1 | ~0.150 s | ~0.114 s |
| 2 | ~0.199 s | ~0.101 s |
| 4 | ~0.210 s | ~0.126 s |
| 8 | ~0.246 s | ~0.133 s |

两台系统在 QEMU TCG 下多线程都没有明显加速（8 线程相对 1 线程约
1.6x vs 1.2x），说明该现象主要来自 TCG/宿主，而非内核调度器；
内核仍比 Alpine 慢约 1.3~1.9x，但远不到数量级。

### 2026-08-06 追加：8 线程共享 pthread mutex 自增（futex 路径）

8 个线程反复 lock/unlock 同一把 pthread mutex，共享计数器 +1，
直到达到目标值；两台系统各自独占 QEMU：

| 目标值 | 内核 | Alpine Linux | 比值 |
|---:|---:|---:|---:|
| 200,000 | 0.150 s | 0.059 s | 2.5x |
| 500,000 | 0.178 s | 0.116 s | 1.5x |
| 1,000,000 | 0.290 s | 0.238 s | 1.2x |

futex/mutex 路径与 Linux 基本同量级（1.2~2.5x），不是 buildstorm
数量级差距的来源。

### 2026-08-06 追加：单线程系统调用逐项对照（宿主时间戳，各自独占 QEMU）

同一份 `sysbench_host.c`（见 `benchmarks/sysbench_host.c`），gcc -O2：

| 阶段 | 内核 | Alpine | 比值 |
|---|---:|---:|---:|
| getpid ×1M | 1.434 s | 0.663 s | 2.2x |
| clock_gettime ×500K | 1.020 s | 0.051 s | 20x* |
| gettimeofday ×500K | 1.063 s | 0.065 s | 16x* |
| open+close 命中 ×20K | 0.324 s | 0.140 s | 2.3x |
| open+close 未命中 ×10K | 0.178 s | 0.073 s | 2.4x |
| statx ×20K | 0.277 s | 0.082 s | 3.4x |
| fstat ×20K | 0.053 s | 0.024 s | 2.2x |
| readlinkat 未命中 ×10K | 0.169 s | 0.061 s | 2.8x |
| pread 4K ×20K | 0.078 s | 0.043 s | 1.8x |
| pread 64K ×2K | 0.024 s | 0.009 s | 2.7x |
| mmap+munmap ×20K | 0.311 s | 0.244 s | 1.3x |
| mprotect ×20K | 0.070 s | 0.030 s | 2.3x |
| brk ×50K 对 | 2.686 s | 0.512 s | 5.2x |
| getdents /usr/bin ×500 | 0.197 s | 0.348 s | 0.6x |
| pipe 1B ping-pong ×20K | 0.110 s | 0.053 s | 2.1x |
| fork+wait ×100 | 0.081 s | 0.087 s | 0.9x |

`*` Alpine 的 clock_gettime/gettimeofday 走 vDSO，基本不陷入内核；
我们的内核没有 vDSO，每次都是完整 syscall。真正的 syscall 开销差距
可参考 getpid（2.2x）。

### 2026-08-06 追加：8 进程 / 8 线程 syscall 对照（宿主时间戳，各自独占 QEMU）

复用 `benchmarks/sysbench_mp.c`，nproc/threads=8：

| 模式 | 内核 | Alpine | 比值 |
|---|---:|---:|---:|
| 8 进程 getpid | 3.56 s | 1.64 s | 2.2x |
| 8 进程 clock_gettime | 4.71 s | 0.14 s | 33x* |
| 8 进程 statx | 1.98 s | 0.16 s | 12.6x |
| 8 进程 pread 4K | 0.95 s | 0.15 s | 6.5x |
| 8 进程 fstat | 0.44 s | 0.10 s | 4.5x |
| 8 进程 open+close | 1.34 s | 0.18 s | 7.5x |
| 8 进程 mprotect | 0.23 s | 0.05 s | 4.8x |
| 8 进程 mmap+munmap | 0.62 s | 0.26 s | 2.4x |
| 8 线程共享 mm mprotect | 0.46 s | 0.25 s | 1.9x |
| 8 线程共享 mm mmap+munmap | 8.82 s | 3.01 s | 2.9x |

`*` 仍受 vDSO 影响。排除 clock 后，8 进程下 statx/openclose/pread
的锁竞争是当前最明显的差距。
