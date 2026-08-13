```
  本文件完全由 LLM 生成，仅供参考！
```
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

futex/mutex 路径与 Linux 基本同量级（1.2~2.5x）。注意：该结论只针对
轻量 ping-pong 微基准，buildstorm 中的大规模等待语义需要看下方
2026-08-09 的实测结论。

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

## 2026-08-09 用户实测结论：等待轮询是已验证的主要瓶颈之一

- 将 `futex` / `epoll_wait` / `ppoll` 从“suspend 后反复重查”的轮询
  改为真正的阻塞等待后，**复制项目构建测试从 105 min 降到 16 min**，
  约为 Linux 的 2.5 倍。
- 对应提交：`813a850f`（ppoll 阻塞）、`f9d8734d`（epoll 阻塞）、
  以及 futex 阻塞语义相关提交（`95f0057d`、`6e748086` 等）。这是组合
  变更的 A/B 结果，证明等待轮询曾是主要问题之一；尚不能据此量化三类
  syscall 各自的贡献。
- 结论修正：轻量 futex ping-pong 的 1.2~2.5x 不能代表 buildstorm；
  大量 futex/epoll/ppoll 等待在旧实现中会主动让出 CPU、再次被调度、
  重查并继续让出，累积成约 6.6x 的构建差距。这里应称为“数倍”，不应
  仅凭该组结果称为“数量级”。

## 2026-08-09 静态扫描：仍存在的正确性风险与潜在热点

- EventFile 存在独立的正确性问题：它将 `File::readable()` 作为当前
  计数是否非零，`sys_read` 因而在零计数时先返回 `EACCES`，到不了
  `EventFile::read`。正确的接口语义应为 `readable() == true`（访问
  权限）和 `ready_to_read() == count > 0`（当前就绪）。
- EventFile 的直接阻塞 read 仍是 `suspend_current_and_run_next()` 轮询，
  之后应改为与写端同一等待队列上的“入队后重查”。不过 buildstorm 中将
  日志阈值从 1000 改为 10 后从未打印：该计数器每轮都会重置为 0，故只要
  看到一次零计数就会立刻打印；这说明实测未走入该等待分支，而非“轮询仅
  少于 10 次”。因此它当前没有 buildstorm 性能影响的证据，应按正确性
  和通用兼容性处理，不应作为下一轮性能优化的首要目标。
- `flock`/`fcntl(F_SETLKW)` 仍在 `suspend_current_and_run_next()` 轮询。
  此外解锁没有显式唤醒等待者，close 也未见锁记录清理；这一项同时是
  正确性和性能问题。
- `stdio`、`devfs`、net/socket 的若干直接阻塞读路径仍是 suspend 轮询。
  标准输入的 ppoll 已有等待队列，不能将该结论外推为 ppoll 路径仍轮询。
- `find_tree` 仍用 `Vec<String> + remove(0)` 拆路径，理论为 O(n²)，但
  常规路径组件很少，优先级低于等待语义和锁竞争。
- `clock_gettime/gettimeofday` 无 vDSO，应用每次调用都会陷入内核；前文
  约 20x 的对比主要是 Linux vDSO 快路径与 syscall 的端到端差异，不等价
  于内核 syscall 本身慢 20x。是否影响构建仍需统计调用量。
- 页缓存/路径解析仍有多把全局锁，历史 8 进程基准中 statx/openclose/
  pread 有 6~12x 差距。这是可信的后续热点，但应在本轮等待语义修复后
  重跑，并以锁等待统计确认具体归因。

### 待补测例

- `benchmarks/eventbench.c` 现有 eventfd / epoll+eventfd 两段不是有效的
  ping-pong：eventfd 的一次 read 会取走累计计数，但测试固定 read `ITERS`
  次，writer 提前完成后会必然死等。修正为累计读取值直到总和达到 `ITERS`，
  或以第二个 eventfd 做每轮确认后，才能用于阻塞和调度验证。
- 修正源码后重新生成宿主静态二进制 `/tmp/eventbench.static`
  （`riscv64-linux-gnu-gcc -static`），再在内核和 Alpine 运行对照。
- 当前 Alpine rootfs（`/tmp/alpine-writable*.img`）已丢失，initramfs
  内也没有 gcc/apk。静态 ELF 的 syscall 对照不受影响：新的 waitbench
  可由 initramfs 中的 BusyBox 下载执行；需要包管理或可写根文件系统的测试
  仍需恢复 Alpine 可写镜像。

## 2026-08-09 waitbench：阻塞唤醒与 timeout 分解

新增 `benchmarks/waitbench.c`。每一轮写端都必须收到确认后才能进入下一轮，
避免 eventfd 计数合并把多次事件误当成一次；每个 phase 在子进程执行，因此
一个不兼容路径不会遮蔽其余测量。它涵盖 futex、eventfd、ppoll/epoll+eventfd、
ppoll/epoll+pipe，以及空 fd 集合的 1/5/10 ms timeout。

复跑命令（均为 RISC-V、`-m 2G -smp 8`）：

```sh
NET_DEVICE=virtio-net-pci sh benchmarks/run_waitbench.sh linux 2000
EMBEDDED=1 TIMEOUT=120 sh benchmarks/run_waitbench.sh kernel 2000
```

内核模式把同一份静态 C ELF 包入临时 Rust initproc，再经正常 `exec` 启动。
这样不依赖当前尚不完整的网卡 ioctl 或不可靠的串口大文件传输；测试内核在
`benchmarks/build/kernel-rv-waitbench`，不会覆盖根目录的 `kernel-rv`。

| phase | 自定义内核 | Alpine Linux | 内核/Linux |
|---|---:|---:|---:|
| futex strict ping-pong | 25.88 us | 47.94 us | 0.54x |
| ppoll + pipe ping-pong | 31.71 us | 40.88 us | 0.78x |
| epoll + pipe ping-pong | 25.83 us | 48.95 us | 0.53x |
| ppoll timeout 1 ms | 10.038 ms | 1.076 ms | 9.33x |
| epoll timeout 1 ms | 10.021 ms | 1.072 ms | 9.35x |
| ppoll timeout 5 ms | 10.038 ms | 5.086 ms | 1.97x |
| epoll timeout 5 ms | 10.034 ms | 5.084 ms | 1.97x |
| ppoll timeout 10 ms | 10.032 ms | 10.100 ms | 0.99x |
| epoll timeout 10 ms | 10.023 ms | 10.098 ms | 0.99x |

结论：

- 修复后的 futex、ppoll(pipe) 和 epoll(pipe) 阻塞-唤醒路径没有表现出剩余的
  构建级性能差距，因而不应优先重写这些路径。
- timeout 被固定量化到约 10 ms。它会把小于一个 tick 的 ppoll/epoll timeout
  拉长到约 10 ms；先统计 buildstorm 中小 timeout 的次数，再判断它对剩余
  2.5x 的贡献。Linux strace 中 ppoll/epoll 的平均阻塞时间是秒级，不能仅凭
  此微基准断言 timer tick 是构建主因。
- eventfd 相关三个 phase 均失败：空计数 read 返回 `EACCES`，与上面的
  EventFile 静态结论一致。它是明确的兼容性缺陷，也使 eventfd 性能暂不可比；
  修复 `readable`/`ready_to_read` 分离和 read 等待队列后再测。
- 在等待路径已收敛后，已有 8 进程数据中的 `statx`/`open+close`/`pread`
  锁竞争和共享地址空间的 mmap/mprotect 仍是更有证据的 buildstorm 后续方向。

## 2026-08-09 mmbench：共享地址空间 mmap 的缩放差距

新增 `benchmarks/mmbench.c` 和 `benchmarks/run_mmbench.sh`。测试使用同一份
静态 RISC-V ELF，在 QEMU `-m 2G -smp 8` 上运行；参数固定为 5000 loops、
8 workers。Linux 通过 Alpine initramfs 下载运行；自定义内核将 ELF 嵌入临时
initproc，输出镜像为 `benchmarks/build/kernel-rv-mmbench`，不覆盖 `kernel-rv`。

```sh
SMP=8 WORKERS=8 sh benchmarks/run_mmbench.sh linux 5000
EMBEDDED=1 SMP=8 WORKERS=8 sh benchmarks/run_mmbench.sh kernel 5000
```

2026-08-09 后续复核发现，最初的共享线程 phase 将 `ops` 标为 400000，但线程函数
实际只执行 `5000 * 8 = 40000` 次 mmap/munmap。因此该行原始绝对 `us/op` 小了
10 倍；下表已经按实际操作数修正。两边运行的是同一二进制，故内核/Linux 倍率不变。

| phase | 自定义内核 | Alpine Linux | 内核/Linux |
|---|---:|---:|---:|
| mmap+munmap 4K，单线程 | 13.14 us/op | 9.63 us/op | 1.36x |
| 匿名 mmap 1 MiB 后首次触页 | 9.88 us/page | 6.01 us/page | 1.64x |
| mprotect 4K，单线程 | 3.36 us/op | 1.40 us/op | 2.39x |
| mmap+munmap 4K，8 线程共享 mm | 161.3 us/op | 20.8 us/op | 7.76x |
| mprotect 4K，8 线程共享 mm | 2.48 us/op | 1.57 us/op | 1.58x |
| mmap+munmap 4K，8 独立进程 | 2.19 us/op | 1.50 us/op | 1.46x |

`mmap_fault_1m_seq` 的单位是首次触及的每个 4K 页面，包含 1 MiB mapping 的
创建和回收摊销。进程项从 fork 到 wait 计时，因此应作为独立地址空间的端到端
对照，不是孤立 mmap syscall 延迟。线程 mprotect 使用互不重叠的页，避免把
同一 PTE 的数据竞争混入结果。

结论：匿名 mmap 的基础路径只有约 1.36x 差距，独立进程并发也只有约 1.46x；
但多线程共享同一地址空间时，mmap+munmap 的吞吐没有随 8 核提升，反而达到
Linux 的 7.76x。当前应优先审计共享 `MemorySet` 的 VMA 查找/修改锁、页表锁和
跨核 TLB 失效是否在每次 mmap/munmap 上串行执行。mprotect 的共享 mm 差距为
1.58x，仍需优化但不是本轮最突出的缩放问题。

为避免内核将 PID 1 的任一 pthread 退出视为“所有应用完成”，mmbench 的实际
负载在一个子进程运行；所有 phase 结束后得到 `MMBENCH_END status=0 failures=0`。

## 2026-08-09 mmbench：VMA 范围查找优化复测

`MemorySet` 的 mmap 找洞、固定地址冲突检查和 munmap 重叠区域收集改为利用
`BTreeMap::range()` 定位前驱和目标范围，避免从 B 树第一个 VMA 起扫描。mprotect
此前已经使用范围迭代，因此没有改变。内核以相同的 QEMU 配置（RISC-V、`-m 2G`
、`-smp 8`、5000 loops、8 workers）重新构建并运行三次；下表的“优化后”取三次
中位数，优化前为上一节的单次基线。

| phase | 优化前 | 优化后中位数 | 三次优化后样本 (us/op) | 变化 |
|---|---:|---:|---|---:|
| mmap+munmap 4K，单线程 | 13.14 | 12.96 | 12.98, 12.88, 12.96 | -1.3% |
| 匿名 mmap 1 MiB 后首次触页 | 9.88 | 9.78 | 10.00, 9.78, 9.77 | -1.0% |
| mprotect 4K，单线程 | 3.36 | 3.05 | 3.30, 3.05, 3.04 | -9.4% |
| mmap+munmap 4K，8 线程共享 mm | 161.3 | 169.9 | 169.9, 172.7, 167.1 | +5.3% |
| mprotect 4K，8 线程共享 mm | 2.48 | 2.48 | 2.48, 2.52, 2.37 | +0.2% |
| mmap+munmap 4K，8 独立进程 | 2.19 | 无有效复测值 | `nan`, `nan`, `nan` | 不比较 |

三次日志分别为 `/tmp/mmbench-kernel.525222.log`、
`/tmp/mmbench-kernel.525582.log`、`/tmp/mmbench-kernel.526074.log`，每次均输出
`MMBENCH_END status=0 failures=0`。独立进程 phase 的操作全部成功，但该阶段结束
时 `clock_gettime(CLOCK_MONOTONIC)` 的差值稳定输出 `nan`；这是现有计时路径的独立
问题，不能用于比较，也不能据此推断 mmap 失败。

该测例没有验证本次算法优化的主目标：顺序 mmap/munmap 每轮仅留下一个临时 VMA，
`addr == NULL` 不走固定地址冲突检查，munmap 也只命中一个 VMA。因此共享 mmap
仍由 `areas.write()`、页表写锁和远端 TLB 刷新主导，169.9 us/op 与旧基线的差异应视为
QEMU 样本波动，不能归因于范围查找优化。后续需增加“保留大量不相交 VMA，再反复
munmap/mprotect 其中一个高地址 VMA”的密集 VMA 测例，才能量化本次 `O(N)` 到
`O(log N + K)` 的收益。
