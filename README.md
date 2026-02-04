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

