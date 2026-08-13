# Building
TARGET := riscv64gc-unknown-none-elf
MODE ?= debug
KERNEL_ELF := target/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
FS ?= EXT4
ARCH ?= riscv64
# Log level: ERROR, WARN, INFO, DEBUG, TRACE, OFF
LOG ?= OFF
# Initproc: default, busybox, ltp_tests
INIT ?= default
# BOARD
BOARD ?= vf2
SBI ?= rustsbi
CPU_NUM ?= 1

# 添加了新sbi启动选项, 以支持qemu8
bl ?= dft
BOOTLOADER := ../bootloader/$(SBI)-$(BOARD).bin

ifeq ($(bl), new)
	BOOTLOADER := ../bl-new-qemu/$(SBI)-$(BOARD).bin
endif

LA_KERNEL_ELF := target/loongarch64-unknown-none/$(MODE)/os
LA_KERNEL_BIN := $(LA_KERNEL_ELF).bin
LA_MEM ?= 36G

# Building mode argument
ifeq ($(MODE), release)
	MODE_ARG := --release
endif
ifeq ($(FS) , EFS)
	FS_IMG := ../fs_img/$(ARCH)_efs.img
else ifeq ($(FS) , EXT4)
	FS_IMG := ../fs_img/$(ARCH)_ext4.img
endif

LA_FS_IMG := ../fs_img/la64_ext4.img

# KERNEL ENTRY
# KERNEL_ENTRY_PA := 0x80200000

# Binutils
OBJDUMP := rust-objdump --arch-name=riscv64
OBJCOPY := rust-objcopy --binary-architecture=riscv64

build: env $(KERNEL_BIN)

env:
ifeq ($(OFFLINE),)
	export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static; \
	export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup; \
	((rustup target list | grep "riscv64gc-unknown-none-elf (installed)") || rustup target add $(TARGET)) && \
	rustup component add rust-src && \
	rustup component add llvm-tools-preview
endif

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

RV_LINKER := src/arch/riscv/linker-virt.ld
ifeq ($(BOARD),visionfive2)
	RV_LINKER := src/arch/riscv/linker-visionfive2.ld
endif

kernel:
	@echo Platform: $(BOARD)
	@RUSTFLAGS="-Clink-arg=-T$(RV_LINKER) -Cforce-frame-pointers=yes" LOG=$(LOG) INIT=$(INIT) CPU_NUM=$(CPU_NUM) cargo build --target riscv64gc-unknown-none-elf $(MODE_ARG) -Z build-std=core,alloc,compiler_builtins -Z build-std-features=""

clean:
	@cargo clean

run: build run-inner

run-inner:
	@rm -f kernel_output.log
	@qemu-system-riscv64 \
		-machine virt \
		-smp 4 \
		-nographic \
		-bios default \
		-kernel $(KERNEL_ELF) \
		-drive file=$(FS_IMG),if=none,format=raw,id=x0 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
		| tee kernel_output.log

gdbserver: build
	@qemu-system-riscv64 -m 128M -machine virt -nographic -bios default -kernel $(KERNEL_ELF) \
	-drive file=$(FS_IMG),if=none,format=raw,id=x0 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-s -S

gdbclient:
	@riscv64-unknown-elf-gdb -ex 'file $(KERNEL_ELF)' -ex 'set arch riscv:rv64' -ex 'target remote localhost:1234'

.PHONY: build env kernel clean run-inner gdbserver gdbclient
.PHONY: env-la kernel-la build-la run-la run-inner-la

# 此处开始为LA64相关

env-la:
ifeq ($(OFFLINE),)
	export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static; \
	export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup; \
	((rustup target list | grep "loongarch64-unknown-none (installed)") || rustup target add loongarch64-unknown-none) && \
	rustup component add rust-src && \
	rustup component add llvm-tools-preview
endif

# BOARD: qemu (default) or 2k1000
BOARD ?= qemu

LA_LINKER := src/arch/la/linker-virt.ld
ifeq ($(BOARD),2k1000)
    LA_LINKER := src/arch/la/linker-2k1000.ld
endif

$(LA_KERNEL_BIN): kernel-la
	@rust-objcopy --binary-architecture=loongarch64 $(LA_KERNEL_ELF) --strip-all -O binary $@

kernel-la:
	@echo Building for loongarch64, BOARD=$(BOARD)...
	@RUSTFLAGS="-Clink-arg=-T$(LA_LINKER) -Cforce-frame-pointers=yes -Ctarget-feature=-ual" LOG=$(LOG) INIT=$(INIT) BOARD=$(BOARD) CPU_NUM=$(CPU_NUM) cargo build --target loongarch64-unknown-none $(MODE_ARG) -Z build-std=core,alloc,compiler_builtins -Z build-std-features=""

build-la: env-la kernel-la

run-la: build-la run-inner-la

run-inner-la:
	@rm -f kernel_output.log
	@qemu-system-loongarch64 \
		-m $(LA_MEM) \
		-machine virt \
		-nographic \
		-kernel $(LA_KERNEL_ELF) \
		-drive file=$(LA_FS_IMG),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		| tee kernel_output.log
