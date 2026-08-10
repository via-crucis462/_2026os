# Linux v6.18 虚拟地址空间与页表锁：关键约束中文译注
本文由 GPT 5.6 Terra 翻译得到。

原文：[`process_addrs.rst`](process_addrs.rst)，来自 Linux v6.18。

本文翻译并归纳原文中与 VMA、页表、`mmap()`/`munmap()` 并发相关的锁约束；
它不是对 909 行原文的逐段翻译。原文行号均指同目录的副本。

## 1. 首先区分两类对象

原文 35--40 行的核心结论是：VMA 元数据的锁**不直接锁住**它描述的内存，
也**不直接锁住**映射该内存的页表。锁的首要作用是让 VMA 在 mm 的索引树中
保持稳定：它不会被删除，也不会在使用期间被不受控地改写（64--69 行）。

因此，正确性不能简化为“持有 VMA 锁就可以任意读写 PTE”。每次操作都要同时
满足两件事：

1. 以合适的 VMA/mm/rmap 锁确认地址空间语义和 VMA 生命周期；
2. 以页表层级的锁和原子 PTE 操作处理页表并发。

## 2. 锁对象及其职责

| 锁 | 粒度与取得方式 | 主要职责 |
| --- | --- | --- |
| `mm->mmap_lock` | 每个 mm 一个读写信号量；`mmap_read_lock()` / `mmap_write_lock()` | 稳定整个地址空间的 VMA 索引和大多数 VMA 元数据。 |
| VMA 锁 | 单个 VMA；读锁 `lock_vma_under_rcu()`，写锁 `vma_start_write()` | 让 page fault 等路径只稳定一个 VMA，而不必占用全局 mmap 读锁。写 VMA 前必须先持有 mmap 写锁。 |
| rmap 锁 | `anon_vma` 或文件 `address_space` 的反向映射树 | 从 folio 反查 VMA 时稳定 VMA。匿名映射用 `anon_vma` 锁，文件映射用 `i_mmap` 锁。 |
| `mm->page_table_lock` | 一个 mm | 保护更高层页表（PGD/P4D/PUD）的修改，以及一部分相关状态。 |
| PMD/PTE 锁（`ptl`） | PMD 或一张 PTE 页 | 细粒度串行化 PTE 页的修改；PTE 常经 `pte_offset_map_lock()` 取得。 |
| RCU | 读侧宽限期 | 保护无锁/乐观遍历时 VMA 或已摘除页表页的对象寿命；它不等价于 PTE 排他锁。 |

原文 45--62、499--526 行。

## 3. VMA 的读写规则

### 读 VMA 元数据，或只要求 VMA 不消失

以下任一方式均可（74--85 行）：

1. 持有 `mmap_read_lock(mm)`；
2. 在 RCU 下尝试 `lock_vma_under_rcu()`；它可能失败，失败后必须回退为 mmap 读锁路径；
3. 先取得相应 rmap 锁，在被锁定的反向映射区间树中找到 VMA。

### 改写 VMA 元数据

大多数 VMA 字段需要同时满足（87--102 行）：

1. `mmap_write_lock(mm)`；
2. 对每个将修改的 VMA 执行 `vma_start_write(vma)`；
3. 若要改任意字段，还要取得 rmap 写锁，使 VMA 对反向映射查找不可见。

锁序不可反过来：可以在 mmap 读锁下取得 VMA 锁；不能已经持有 VMA 读锁后再等
mmap 写锁，否则另一个持有 mmap 写锁、正等待该 VMA 写锁的线程会形成死锁
（126--129 行）。

原文强调：没有目标 VMA 的写锁时，page fault 可以与当前操作并发（107--109 行）。
这正是“mmap 写路径与不相关 VMA 的 fault 可以并行”的基础；它不是允许两个
`mmap_write_lock()` 持有者并行修改同一个 mm。

## 4. 页表操作的分类

原文 301--356 行将操作分为四类：

| 操作 | VMA 稳定性要求 | 额外要求 |
| --- | --- | --- |
| 遍历页表 | 任一可稳定 VMA 的锁；少数专门快路径可无锁 | 处理页表页释放与 PTE 原子读取。 |
| 安装/填充 PTE | 必须持有 mmap 或 VMA 锁；**仅 rmap 锁不够** | 上层页表锁、PTE 锁，以及必要的重新验证。 |
| zap/清 PTE | 只需让 VMA 保持稳定 | 实际写 PTE 时仍要取得对应页表锁。 |
| 释放页表页 | 调用者必须已 zap，并阻止新 fault 或新 PTE 安装 | 目标范围不得仍可经 rmap 到达。 |

“安装 PTE”与“zap PTE”要求不同，是理解 `munmap()` 窗口的关键：zap 可以只依赖
稳定 VMA，但在旧 PTE 清除后、页表页释放前，任何重新填 PTE 的路径都必须已经被
阻止。

## 5. PTE 锁与原子性规则

原文 531--631 行给出的基本规则如下：

1. 修改一个 PTE 时，必须持有该页表页的锁；唯一例外是能证明没有任何并发访问者的
   内部清理，例如特定的 `free_pgtables()` 阶段。
2. PTE 的读写必须满足架构规定的原子性。页表锁不能阻止硬件并发更新 accessed/dirty
   位，也不能替代单次读取和原子 read-modify-write 操作。
3. PTE 读取使用 `pXXp_get()` 一类封装，以 `READ_ONCE` 防止编译器重复或重排读取。
   对旧值有依赖的修改应使用硬件原子操作，例如 `ptep_get_and_clear()`。
4. PTE 写入和清除分别使用 `set_pXX()` 与 `pXX_clear()` 一类架构封装。
5. PTE 页可能已从 PMD 脱链、等待 RCU 回收。写者通常必须在取得 PTE 锁后重新验证
   PMD 仍指向同一张 PTE 页（565--585、684--695 行）。

`pte_offset_map_lock()` 做的不只是“拿一把 PTE 锁”：它处理 PTE 页映射、RCU、
取得 PTE 锁，并在需要时验证上层 PMD；释放配对为 `pte_unmap_unlock()`
（660--664 行）。

## 6. 为什么不能在 munmap 的窗口中重新填 PTE

原文 547--555 行的警告可译为：`vms_clear_ptes()` 在 `unmap_vmas()` zap PTE 与
`free_pgtables()` 释放页表之间存在窗口。这时 VMA 仍可能在 rmap 树中可见；而
`free_pgtables()` 假定 zap 已完成，会无条件清理该范围的页表。如果此窗口安装了
新的 PTE，会造成内存泄漏或其他危险行为。

所以“PTE 为空”不代表任何持有 rmap 锁的路径都可以随意填入。填入空 PTE 必须持有
mmap 或 VMA 锁，并满足该路径的 VMA 仍有效、不会被这次 unmap/free 接管的条件。

## 7. Linux munmap 的发布和拆除顺序

原文 389--394 行明确描述了这个顺序：`munmap` 在 mmap 写锁下先摘除 VMA，随后把
`mmap_lock` 降级为读锁，再拆除页表。

可概括为：

```text
mmap 写锁 + 目标 VMA 写锁
  -> 等待已有 VMA 读者离开，并使目标 VMA detached
  -> 从 VMA/Maple Tree 发布删除结果
  -> mmap 写锁降级为读锁（没有完全释放）
  -> 对 detached VMA zap PTE、回收页表、完成 TLB 处理
  -> 释放 mmap 读锁
```

该顺序的锁含义是：

- 已经持有旧 VMA 读锁的 fault 会在 VMA 写锁阶段被等待；
- 新 fault 查找不到已 detached 的 VMA，不能为旧映射重新安装 PTE；
- 新的映射修改需要 mmap 写锁，因此不能越过降级后仍持有的 mmap 读锁；
- PTE 修改本身继续受 PTE 页锁串行化。

注意：这里的 mmap 读锁是全 mm 的映射写者屏障，不是按 `[start, end)` 的范围锁。
Linux 不使用“持有到 TLB 完成的 per-range reservation”来实现该语义。

## 8. VMA 锁的实现要点

原文 753--855 行的关键机制：

- VMA 读锁是乐观的。`lock_vma_under_rcu()` 在 RCU 读侧查 Maple Tree，尝试取得
  VMA 读锁；发生竞争或已开始写入时失败，调用者回退。
- 成功的 VMA 读锁通过 `vm_refcnt` 持有引用，`vma_end_read()` 释放。
- VMA 写锁必须已经持有 mmap 写锁。写者设置不可与读者修改的 refcount 位，等待
  所有 VMA 读者离开，然后使 `vma->vm_lock_seq` 与 `mm->mm_lock_seq` 相等。
- 两个 sequence count 相等表示该 VMA 在本次 mmap 写锁周期中处于写锁状态。
- `mmap_write_unlock()` 或 `mmap_write_downgrade()` 会结束所有 VMA 写锁，并推进
  mm 的 sequence count；因此没有独立的 `vma_end_write()`。

这不是“只在 VMA 上放一个 seqcount”。正确性还依赖读者引用计数、写者等待读者、
RCU 查找、以及 mmap 写锁的生命周期约束。

## 9. mmap 写锁降级语义

原文 857--902 行：`mmap_write_downgrade()` 先结束全部 VMA 写锁，再把 mmap 写锁
降为读锁；它没有把 mmap 锁完全放开，因此地址空间仍稳定。

锁冲突矩阵如下（Y=互斥，N=可共存）：

| 已持有 \\ 请求 | 普通读 R | 降级读 D | 写 W |
| --- | --- | --- | --- |
| R | N | N | Y |
| D | N | Y | Y |
| W | Y | Y | Y |

两个 D 不能共存，因为第二个操作必须先取得 W 才能降级，而第一个 D 阻止 W。
普通读者可与 D 共存，但映射写者不能。

## 10. 对本项目的直接含义

以下是将原文约束映射到本项目时应保持的区别，而不是声称现有类型已经等价：

- `MemorySet::areas.write()` 目前同时承担 VMA 树修改和全局映射写者排他的职责，
  因而比 Linux 的“mmap 写锁 + per-VMA 锁”更粗。
- `VersionedArea.sequence` 目前不是 Linux 的 VMA 锁等价物；若要在 `areas` 锁外
  使用它，仍需要读者 pin、detaching 状态、写者等待和重试协议。
- `RangeLock` 是本项目的额外设计，不是 Linux 的通用 mm 范围锁。它若被用于允许
  非重叠映射操作并发，所有会安装/删除/改权限/重用同一 VA 范围的路径都必须遵守它。
- `page_table.write()` 是全局页表写锁；它比 Linux 的 PTE 页级 `ptl` 粗。仅释放
  `areas.write()` 不会让不同 PTE 页的修改真正并行。
- 无论锁粒度如何，撤销映射的帧都必须在 PTE 清除和对所有可能运行该 mm 的 CPU
  完成 TLB 失效之后才释放；这是页表锁之外的硬件可见性要求。

## 11. 实现检查表

当新增一个会处理用户页表的路径时，至少逐项回答：

1. 用哪一种锁让目标 VMA 稳定？
2. 若会填入 PTE，是否持有 mmap/VMA 锁，而不只是 rmap 或裸页表锁？
3. 对哪个页表页取得了锁？上层页表/PTE 页被并发替换后是否重新验证？
4. 若会释放页表页，是否已 zap，且已阻止所有重新填 PTE 的途径和 rmap 可达性？
5. 若会解除映射并回收帧，TLB 失效是否先于帧释放完成？
6. 新增锁是否遵守锁序：`mmap_lock` 在 VMA/rmap/PTE 锁之前？

