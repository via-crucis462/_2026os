export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup

MODE ?= debug
RV_SMP ?= 1
LA_SMP ?= 1
RV_GDB_PORT ?= 1234
LA_GDB_PORT ?= 1235
# default, sh, ltp
INIT ?= default
# virt (qemu) or 2k1000 (real board)
BOARD ?= virt
RV_ELF ?= os/target/riscv64gc-unknown-none-elf/$(MODE)/os
LA_ELF ?= os/target/loongarch64-unknown-none/$(MODE)/os

GDB_MUL_EXITS = $(shell command -v gdb-multiarch)

ifneq ($(GDB_MUL_EXITS),)
	GDB = gdb-multiarch
else
	GDB = gdb
endif

all: prev build-user copy-user build copy

prev: 
	cd os && $(MAKE) env
	cd os && $(MAKE) env-la

build-rv:
	cd os && $(MAKE) build MODE=$(MODE) LOG=$(LOG) INIT=$(INIT)
build-la:
	cd os && $(MAKE) build-la MODE=$(MODE) LOG=$(LOG) INIT=$(INIT) BOARD=$(BOARD)
ifeq ($(BOARD),2k1000)
	@echo "  -> Packing uImage for 2K1000..."
	cd os && cp target/loongarch64-unknown-none/$(MODE)/os ../kernel-la-$(BOARD)
	python3 boot/build_uimage.py kernel-la-$(BOARD) kernel-la-$(BOARD).uImage
	@echo "  -> Making binary for 2K1000..."
	rust-objcopy -O binary kernel-la-2k1000 kernel-la-2k1000.bin
	mkdir -p ~/loongson/tftproot && cp -f kernel-la-2k1000.bin ~/loongson/tftproot
	mkdir -p ~/loongson/tftproot && cp -f kernel-la-2k1000.uImage ~/loongson/tftproot
endif

build-user-rv:
	cd user && $(MAKE) build ARCH=riscv64
build-user-la:
	cd user && $(MAKE) build ARCH=loongarch64
build-user: build-user-rv build-user-la

copy-rv:	
	cd os && cp target/riscv64gc-unknown-none-elf/$(MODE)/os ../kernel-rv
copy-la:
	cd os && cp target/loongarch64-unknown-none/$(MODE)/os ../kernel-la-$(BOARD)
ifeq ($(BOARD),2k1000)
	@echo "  -> Packing uImage for 2K1000..."
	python3 boot/build_uimage.py kernel-la-$(BOARD) kernel-la-$(BOARD).uImage
	@echo "  -> Making binary for 2K1000..."
	rust-objcopy -O binary kernel-la-2k1000 kernel-la-2k1000.bin
endif

copy-user-rv:
	cd user && find target/riscv64gc-unknown-none-elf/release/ -maxdepth 1 -name 'initproc*' ! -name '*.*' -exec cp -f {} ../os/src/arch/riscv/ \;
copy-user-la:
	cd user && find target/loongarch64-unknown-none/release/ -maxdepth 1 -name 'initproc*' ! -name '*.*' -exec cp -f {} ../os/src/arch/la/ \;
copy-user: copy-user-rv copy-user-la

copy: copy-rv  copy-user-rv copy-la  copy-user-la

build: build-rv build-la

test-rv: MODE = release
test-rv: build-user-rv copy-user-rv build-rv copy-rv
	@rm -f kernel_output.log
	@qemu-system-riscv64 -machine virt \
	-kernel kernel-rv \
	-m 1G -nographic -smp $(RV_SMP) \
	-bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-no-reboot \
	-device virtio-net-device,netdev=net0 \
	-netdev user,id=net0,hostfwd=udp::6200-:2000,hostfwd=tcp::6200-:2000 \
	-rtc base=utc \
	| tee kernel_output.log

test-la: MODE = release
test-la: build-user-la copy-user-la build-la copy-la
	@rm -f kernel_output.log
	@qemu-system-loongarch64 \
	-kernel kernel-la-$(BOARD) \
	-m 1G -nographic \
	-smp $(LA_SMP) \
	-drive file=sdcard-la.img,if=none,format=raw,id=x0 \
	-device virtio-blk-pci,drive=x0 \
	-no-reboot \
	-device virtio-net-pci,netdev=net0 \
	-netdev user,id=net0 \
	-rtc base=utc \
	| tee kernel_output.log

test-la-2k1000: MODE = release
test-la-2k1000: BOARD = 2k1000
test-la-2k1000: build-user-la copy-user-la build-la copy-la
	@rm -f kernel_output.log
	@qemu-system-loongarch64 \
	-machine virt \
	-cpu la464 \
	-kernel kernel-la-2k1000 \
	-m 1G -nographic \
	-smp $(LA_SMP) \
	-drive file=sdcard-la.img,if=none,format=raw,id=x0 \
	-device virtio-blk-pci,drive=x0 \
	-no-reboot \
	-device virtio-net-pci,netdev=net0 \
	-netdev user,id=net0 \
	-rtc base=utc \
	| tee kernel_output.log

debug-rv: MODE = debug
debug-rv: build-user-rv copy-user-rv build-rv copy-rv
	@rm -f kernel_output.log
	@qemu-system-riscv64 -machine virt \
	-kernel kernel-rv \
	-m 1G -nographic -smp $(RV_SMP) \
	-bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-no-reboot \
	-device virtio-net-device,netdev=net \
	-netdev user,id=net \
	-rtc base=utc \
	-semihosting-config enable=on,target=native \
	-S -gdb tcp::$(RV_GDB_PORT) \
	-monitor tcp::1236,server,nowait \
	| tee kernel_output.log

debug-la: MODE = debug
debug-la: build-user-la copy-user-la build-la copy-la
	@rm -f kernel_output.log
	@qemu-system-loongarch64 \
	-machine virt \
	-kernel kernel-la \
	-m 1G -nographic \
	-smp $(LA_SMP) \
	-drive file=sdcard-la.img,if=none,format=raw,id=x0 \
	-device virtio-blk-pci,drive=x0 \
	-no-reboot \
	-device virtio-net-pci,netdev=net0 \
	-netdev user,id=net0 \
	-rtc base=utc \
	-S -gdb tcp::$(LA_GDB_PORT) \
	-monitor tcp::1237,server,nowait \
	| tee kernel_output.log

gdb-rv:
	@$(GDB) $(RV_ELF) \
	-ex "set confirm off" \
	-ex "set pagination off" \
	-ex "set print thread-events off" \
	-ex "set scheduler-locking off" \
	-ex "set schedule-multiple on" \
	-ex "target extended-remote :$(RV_GDB_PORT)" \
	-ex "info threads" \

#	-ex "b os::syscall::process::sys_exec"
	
#	-ex "b os::syscall::fs::sys_dup2"
    

gdb-la:
	@$(GDB) $(LA_ELF) \
	-ex "set confirm off" \
	-ex "set pagination off" \
	-ex "set print thread-events off" \
	-ex "set schedule-multiple off" \
	-ex "set scheduler-locking on" \
	-ex "set tdesc filename tools/la-gdb/loongarch64-fpu-lsx-lasx-lbt.xml" \
	-ex "target remote :$(LA_GDB_PORT)" \
	-ex "info threads"
