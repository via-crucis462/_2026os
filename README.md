# (队伍名)os
## 简介
- 此项目是北京科技大学2026(队伍名)的os项目，基于rCore ch7框架改编而来的。。

# 日志
## 2026.1.25
1. 合并了munmap、brk、mmap的已完成部分，添加了相关系统调用定义和有关方法，但其功能尚未完善，目前的改动保证了不影响内核原有功能。
2. 对部分模块添加了pub属性。
3. 将syscall参数数量由4个改为6个。
## 2026.1.27
1. 修改了brk的实现，已过测。
2. memory_set添加了brk_index记录堆区索引（并在初始化等过程中维护），便于在mmap等操作中跳过堆区及之前的部分。
3. 修改了部分注释的表述，使其更清晰。
4. mmap仍未完成，munmap已基本完成，但未测试。
5. 准备开始完成其他系统调用的实现。
## 2026.1.30
1. 调整了先前munmap等的调用层次，优化代码结构。
2. 开始实现clone。在不影响原有功能的前提下，将原本不带参数的fork改为了带参数的clone，实现了部分功能。
3. 对原有框架中的fork进行了修改。
## 2026.1.31
1. 初步实现wait4，已过测。但目前的实现十分简陋，仅实现WNOHANG（默认）且不完全符合规范。
2. 在wait4实现后，clone也成功过测。
3. ！！！引入bug：按测例修改后的ch7b_initproc会反复报错（不影响测试，只是运行用户程序需要按两次回车），下面给出测试时的部分输出：
```bash
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
[initproc] Released a zombie process, pid=1, exit_code=0
========== START test_wait ==========
[K] do_clone: func=0x11, stack=0x0, flags=0xc
wait child success.
wstatus: 0
========== END test_wait ==========

>> >> test_wait passed.
```
## 2026.2.1
合并后wait的bug已解决


## 2026.2.2
1. 完成了EXT4文件系统镜像的挂载，读取，执行。
2. 完成了第一次合并，做完了许多系统调用。

## 2026.2.3~未知
TODO：把现有的内核架构进一步拆分为抽象层和实现层，其中实现层在arch下分架构实现（注：难度较高，正在尝试完成，预计耗时数天），以保证先前工作不需要针对不同架构进行修改。特别的，对于内存，预期目标是maparea和memory_set等结构体不需要针对龙芯进行重新定义、实现。

## 2026.2.3_fmx
！！！注意：目前arch/la/下的代码均为riscv版本的粘贴，完全无法工作，请不要尝试运行。
1. 初步完成项目架构的调整，将架构强相关代码移到arch/riscv/下，并在arch/la/下创建了对应的文件结构但尚未实现。
2. 虚拟内存模式由SV39改为了SV48，使其与LA64一致。带来的影响：页表层数由3改为4，但页表项中PPN的位域划分有所不同，相关转换函数均已修改。
3. 部分结构体的定义被单开文件存储，例如flags.rs等。
4. 部分不在arch/riscv/下的代码，涉及到riscv特性的部分添加了条件编译。

## 2026.2.3_tbw
1. 完成了fstat的系统调用书写
2. 修改了makefile，完全去除了rCore中对于easy_fs的支持，改为ext4
3. 完善了open系统调用的书写，支持了未创建文件的创建，修改了Openflag，符合posix标准。
4. 因为open符合了posix标准，所以close这个测例也过了。

## 2026.2.4_tbw
1. 完成了mkdir的系统调用书写，但由于创建后没有删除，故第一次会显示success，第二次会段错误（posix标准返回-1，而返回-1报错）
2. 将Inode中的i_size从逻辑块数改为了字节数
3. 修改了vfs目录项定义，使其满足posix标准
4. 修改了主函数中list_app()中的实现逻辑，删除了ls和init调用，改为用getdents()代替
5. 完成了sys_getdents()


## 2026.2.4_fmx
1. 将riscv的内存模式改回了SV39，la64进行了相应调整。
2. 基本完成了LA64的接口转换，例如，实现了权限位转换，三级页表定义等，还大幅调整了文件结构。接下来，理论上只需要少量修改即可完成LA64的内存管理功能，因为目前的实现保证了la虚拟地址的结构与riscv几乎完全一致，三级页表遍历的函数可以直接复用，而两者的页表项结构也较为类似。
3. tlb缺页异常处理函数尚未实现，寄存器初始化函数也只是雏形。

## 2026.2.5
### tbw
1. 当ext4的inode指针中数据块号为0时，并不代表该块不参与elf数据块大小的计算，而真实的elf大小应该在读取Inode的size块后才得已知道。所以，更改了原read_at的设计，使其符合标准做法。因此通过了测试用例clone
2. 修改了ext4inode中对于block_size=4096等写法，使其等于全局变量BLOCK_SZ
### fmx
1. 尝试将rust工具链版本调整到nightly-2026-01-01，修改了部分语法以适应新版本编译器，例如删除了一些feature，在部分强制类型转换中间加入as *const()等。尽管经过简单测试发现测例能正常运行，但暂不建议和主分支合并。
2. 在新版编译器下，链接器报错大幅度减少且更准确，根据报错调整部分符号后通过了编译链接，LA64理论上能运行helloworld了，但由于qemu没装好，还没测。
3. 暂未调整第三方库的版本，因为如果调整，要做的修改过多。

## 2026.2.6
### tbw
1. 实现了getcwd和chdir：在数据结构tcb中，加入了cwd字段用于存储当前目录的缓存，Dentry特性。
2. 实现了getcwd和chdir，但目前的搜索文件树仍不够健壮，对于.和..的处理不够标准
3. 实现了mmap和munmap，在memory_set中添加了一个函数名为find_free_area，调用时当传入的虚拟地址有冲突时，调用这个函数似乎可以自动分配内存
4. 给SuperBlock中添加了字段，用于记录其是否开启了extents扩展。
5. 增加了alloc_blockid方法给EXT4Inode，用于动态分配文件的块大小内容。

### fmx
1. 添加目录bl-new-qemu/，其中包含了最新版的rustsbi以提供对qemu8（如果在ubuntu24.04下用apt直接安装，即为这个版本）的支持。之所以希望使用qemu8，是因为希望在涉及la的场景中保持环境较新，且apt安装的qemu8.2.2包含了la版本，无需额外设置。makefile中也做了对应修改，现在，如果你尝试使用qemu8启动riscv版本，请使用make run bl=new。请注意：sbi.rs中的系统调用号需要做相应调整（其实就是改两个数字的事，这里已经提前写好并注释）。考虑到之前的riscv主要使用qemu7，所以这里没有应用修改，使用qemu7并正常使用make run即可。
2. 发现将riscv版本的linker.ld和entry.asm直接复制到la，按la语法重写entry.asm，再修改基地址，就能直接正常启动。按此思路完成了la的最小化裸机启动配置。
3. 验证了先前实现的la uart输入输出，成功在龙芯版qemu上打印出helloworld。

## 2026.2.7
### fmx
1. 修改la的trap.S，实现了app初始化，完善TC结构体的设计与处理，完善trap模块到理论可用（未测试），封装了对sp和a0a1寄存器的读写为统一接口。
2. trap应该能实现内存空间的切换了，学习了有关滑动窗口的知识，准备使用映射窗口将内核空间调整至usize:MAX / 2 + 1起的“平移”映射。
3. todo：跳板暂未完善。

## 2026.2.8
### fmx
继续修改la的兼容层，发现跳板几乎无需修改。修改main函数尝试运行，成功打印remap_test passed!但然后死循环。

## 2026.2.9
### tbw
1. 伪实现了mount，unmount
2. 实现了openat，修改了原open系统调用，现在接收第一个参数
3. 实现了unmae系统调用，在process.rs中，只支持常量输出目前

## 2026.2.11
### tbw
1. 完成了busybox的运行环境配置，修改了内存相关，仍需阅读一下
2. 完成了对于符号链接的识别运行。

## 2026.2.19
### fmx
完善了pci扫描和初始化，la版本现在能成功编译运行，但pci有关驱动仍需完善。

## 2026.2.20
pci驱动能跑了
 
## 2026.2.21
优化la64 pci驱动部分，尝试运行la64 shell程序但失败

## 2026.2.22&23
经过大量调试修改，现在switch逻辑跑通了，restore时遇到内存问题。ranslate时，没有成功从用户提供的token出找到数据。
另外，因为硬件会自动处理，取消了龙芯硬件窗口映射时的页表机制。

## 2026.2.24&25
大量调试后解决了页表相关问题。阅读文档注意到目录项没有权限位，直接存储下一级页表基址。修复后tlb重填能正确获取pt地址。
```bash
    [kernel] csr_info: TLBRELO0 = 0x324c191
    [kernel] csr_info: TLBRELO1 = 0x324f191
```

## 2.27~3.2
终于解决了卡死问题，现在进入用户程序后正常syscall，但任务执行有关实现还存在问题。

## 3.3
1. 通过刷新tlb解决了先前的问题，不过还能通过更新asid提高性能。
2. basic基本过测，个别系统调用与riscv不同，例如statx，需要实现。

## 3.5
### tbw
1. 完成了newfstat，access，exec的改写，以及pread64和statx
2. newfstat实际上是fstat的进化，但实际上本身之前定义的Stat已经符合要求，所以直接调用fstat即可
3. access是用于测试是否能成功打开文件，第一个参数用于指示寻址方式，实际上，只需要看openfile是否能成功就行，成功返回1，失败返回0
4. pread64更是ez，只是指定位置read但不改变文件本身的读取记录，只需要在read时将OSinode中的inner的偏移删掉即可
5. exec的改写，目前，经过符号链接转译后，判断结尾是否有.sh。若有，则运行任务时，task的runtask的第一个参数中添加/bin/busybox sh
6. 对于符号链接的解析：目前find_tree提供选项，判断符号链接是否需要进行转译。
7. statx的第一个参数类似access指示寻址方式，大概介绍一下
- 如果传入的路径以/开头，则dirfd没用
- 如果dirfd 等于-100，则第二个参数，即传入的路径作为相对于当前工作目录的路径
- 如果dirfd是正数，则作为文件描述符，第二个参数传入的路径是相对于这个文件描述符对应的文件的位置进行寻址
8. find_tree自动解析符号链接
### 什么是符号链接？
1. linux的任何文件都有自己唯一的一个inode号，且inode号也一一对应一个文件，包括符号链接
2. 符号链接里存的是它指向的文件的路径。例如，符号链接sh里存的就是./busybox。
3. Windows里叫快捷方式（绷

### fmx
1. 主要实现了prctl和随机数生成等系统调用，伪实现了几个涉及多线程的系统调用。
2. 关于多线程：如果同一个任务开了多个tread，他们会使用同一套memory_set，为了实现真正的多线程我们需要对共享内存进行管理（ 锁）。
3. 关于多用户/权限：目前，我们的内核未对用户程序进行权限限制，也没有多用户的概念，每个用户程序都能call所有系统调用，后续可能需要管理。
### 关于prctl
1. 全名process control，进程管理。
2. 这个系统调用主要用于进程向内核请求：对自己修改一些权限/线程相关的特性；获取某些特性的状态。
3. 选项过多，具体可见crate::syscall:::prctl。
4. 针对尚未实现的特性，根据其特点选用不同的返回值来尽可能贴合用户程序的需要。