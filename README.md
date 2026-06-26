# ShellCore

![alt text](docs/ustb-logo.png)

## 简介

`ShellCore`是由三位队员基于[2025春夏季开源操作系统训练营 rCore-Tutorial-v3 ch7](https://github.com/rcore-os/rCore-Tutorial-v3/tree/ch7)逐步开发而来的操作系统内核。

## 参赛文档

初赛阶段的项目介绍可以在[初赛文档](docs/初赛文档.md)查看。

除commit记录外，[docs](docs/)目录下还有[初赛阶段日志](docs/初赛阶段日志.md)，其记录了本项目2026年1月至5月的开发过程。

## 使用说明

通过

```sh
INIT=sh make test-rv
```

和

```sh
INIT=sh make test-la
```
分别可编译riscv和loongarch版的用户程序及内核，并通过qemu启动。上述参数下，我们的内核会挂载sdcard-*.img镜像为根目录，初始化程序会启动镜像中位于`/musl/busybox`的busybox的shell。

此外，可以通过如下变量调整编译设置：

```makefile
# rv核心数
RV_SMP ?= 1
# la核心数
LA_SMP ?= 1
# 日志等级: ERROR, WARN, INFO, DEBUG, TRACE, OFF
LOG ?= OFF
# 初始化程序：default, sh, ltp，分别会运行全量测试，busybox shell，ltp特定测试
INIT ?= default
```

通过合适的启动参数与对[初始化程序源码](/home/asta/_2026os/user/src/bin)的修改，可以在运行make-test-*时让初始化程序运行其他程序。

---

此外可以通过

```sh
make debug-rv

make debug-la

make gdb-rv

make gdb-la
```

启动调试。需要先运行`make debug-*`再运行`make gdb-*`。