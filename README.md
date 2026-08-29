# ShellCore

![ShellCore logo](docs/ustb-logo.png)

## 简介

`ShellCore` 是三位队员基于 [2025 春夏季开源操作系统训练营 rCore-Tutorial-v3 ch7](https://github.com/rcore-os/rCore-Tutorial-v3/tree/ch7) 逐步开发而来的 SMP 架构类 unix 操作系统内核。

项目获得了[2026年全国大学生计算机系统能力大赛-操作系统设计赛(全国)-OS内核实现赛道](https://os.educg.net/#/index?TYPE=26OS_K)**国家级二等奖**，可见[2026年全国大学生计算机系统能力大赛操作系统设计赛全国总决赛获奖名单公告](https://gitlab.eduxiji.net/csc1/csc-os/os2026/-/blob/main/2026%E5%B9%B4%E5%85%A8%E5%9B%BD%E5%A4%A7%E5%AD%A6%E7%94%9F%E8%AE%A1%E7%AE%97%E6%9C%BA%E7%B3%BB%E7%BB%9F%E8%83%BD%E5%8A%9B%E5%A4%A7%E8%B5%9B%E6%93%8D%E4%BD%9C%E7%B3%BB%E7%BB%9F%E8%AE%BE%E8%AE%A1%E8%B5%9B%E5%85%A8%E5%9B%BD%E6%80%BB%E5%86%B3%E8%B5%9B%E8%8E%B7%E5%A5%96%E5%90%8D%E5%8D%95%E5%85%AC%E5%91%8A.pdf)。


## 项目介绍

项目结构如下：

```
.
├── Makefile                      # 顶层构建入口：make test-rv / test-la / debug-* 等
├── os/                           # 内核主体（RISC-V 与 LoongArch 双架构）
│   ├── src/
│   │   ├── arch/                 # 架构相关代码（riscv/、la/）
│   │   ├── drivers/              # 磁盘、网络、串口等设备驱动
│   │   ├── ext4fs/               # ext4 文件系统实现
│   │   ├── fs/                   # 文件系统抽象、页缓存与块缓存
│   │   ├── mm/                   # 内存管理：地址空间、页表、RCU 等
│   │   ├── process/              # 线程组 / 进程模型与调度器
│   │   ├── sync/                 # 锁、等待队列等同步原语
│   │   ├── ipc/                  # 进程间通信
│   │   ├── net/                  # 网络协议栈
│   │   ├── syscall/              # 系统调用层
│   │   ├── auth/                 # 权限与用户身份
│   │   └── ...
│   ├── build.rs
│   └── Cargo.toml
├── user/                         # 用户态程序与初始化程序
│   └── src/bin/                  # initproc 等初始化程序
├── boot/                         # uImage 打包脚本等启动相关内容
├── toolkits/                     # 开发板烧写、测评镜像制作等工具
├── benchmarks/                   # 性能基准与对比测试（mmbench、sysbench、iozone 等）
├── docs/                         # 参赛文档、日志与测试结果
├── sdcard-rv.img / sdcard-la.img # 构建产物：ext4 根文件系统镜像（构建后生成，不入库）
├── kernel-la                     # 构建产物：LoongArch 内核镜像（构建后生成，不入库）
└── ...
```

---

相较于rCore，我们的内核增加了以下特性：

1. 完善的多核并行处理支持，通过锁与原子变量结合各类边界情况约束，保证多核并行的正确性。
2. 具备 COW 语义的 **exec 按需加载与代码段零拷贝**，支持懒分配与完整的 page fault 处理。
3. 地址空间翻译的 **pin 机制**，以及内存管理模型的初步 **RCU 语义**。
4. futex、epoll、ppoll 基于等待队列实现真正阻塞，替代了初赛阶段“睡眠轮询”的过渡语义。
5. **内核工作线程**：将磁盘回写、软件计时器 tick、网络 poll 等周期性工作交由独立的内核工作线程执行，不再占用普通用户进程的 trap 流程。
6. 合并页缓存与块缓存管理，减少磁盘访问的一次拷贝；用更快的**近似 LRU** 替换原有 LRU 算法。
7. 彻底重构 pcb + tcb 的进程线程模型：参考 Linux 的线程模型，删除 pcb 包装，任务通过`线程组`组织为`进程`，跨线程共享的资源以`Arc`管理，并在 clone 时依据 flags 参数决定保留的资源与新旧任务关系。
8. 完整的类 Linux 多队列调度，含 CFS、FIFO 等调度类型，并实现基于“主动任务窃取”和“任务唤醒时优先选择空闲核 + IPI 休眠唤醒”的**负载均衡**模型。
9. 基于核间 IPI 的 **TLB 跨核刷新与按需刷新**。RISC-V 实现用户态共享内核页表：将内核映射至高半地址空间，用户页表除映射用户页外，还通过复制内核高半根页表引用内核页表，以减少 satp 切换及相应的 TLB 刷新开销。
10. 细化各类锁的粒度，实现写者优先（区别于`spin`库的写者不公平策略）的读写锁`RwLock`，并广泛用于页缓存、内存管理结构等读多写少路径，替换原有的`Mutex`。
11. 完成 ext4fs 的多线程并发安全适配与元数据校验码更新。

此外，我们已完成对龙芯和 RISC-V 两款开发板的适配，实现了磁盘与网络驱动，在两款板卡上**成功启动 busybox shell**，并**通过了大部分初赛测例**。特别地，我们研究并解决了龙芯开发板上与 qemu 行为不同的 ALE 问题。

---

在[docs](docs/)下有项目参与[全国大学生计算机系统能力大赛](https://os.educg.net/#/)时期的文档，其中有对项目技术细节/历史优化的详细介绍，这里不再赘述。

## 使用说明

通过

```sh
RV_SMP=8 INIT=sh make test-rv
```

和

```sh
LA_SMP=12 INIT=sh make test-la
```

可分别编译 RISC-V 与 LoongArch 版本的用户程序及内核，并通过 QEMU 启动。

在上述参数下，内核会将 `sdcard-*.img` 镜像（仅支持 ext4 单分区镜像）挂载为 `/`，[初始化程序_shell](user/src/bin/initproc_sh.rs) 会优先启动镜像中的 `/bin/bash` 作为终端；找不到时，则回退到镜像中的 `/musl/busybox sh`。需要镜像的正确位置有动态链接器和动态链接库。

---

可以通过如下变量调整编译设置：

```makefile
# rv核心数
RV_SMP ?= 1
# la核心数
LA_SMP ?= 1
# 日志等级：ERROR、WARN、INFO、DEBUG、TRACE、OFF
LOG ?= OFF
# 初始化程序：default、sh、ltp 等，分别会运行全量测试、busybox shell、ltp 特定测试等
INIT ?= default
```

通过合适的启动参数，并修改[初始化程序源码](user/src/bin)中编码的脚本字符串，即可在 `make test-*` 时运行其他程序。

---

此外，还可以通过以下命令启动调试或连接 gdb（需根据实际情况调整参数）：

```sh
make debug-rv

make debug-la

make gdb-rv

make gdb-la
```

注意需要先运行 `make debug-*`，再运行 `make gdb-*`。