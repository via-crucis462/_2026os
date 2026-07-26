export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup

MODE ?= release
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
TFTP_ROOT ?= $(HOME)/loongson/tftproot
SATA_IMAGE ?= sdcard-la.img
SERIAL_PORT ?=
SERIAL_BAUD ?= 115200
BOARD_IP ?= 192.168.1.20
SERVER_IP ?=
TFTP_TIMEOUT ?= 900
SATA_WRITE_TIMEOUT ?= 900
SATA_VERIFY_TIMEOUT ?= 900

.PHONY: sata sata-verify

GDB_MUL_EXITS = $(shell command -v gdb-multiarch)

ifneq ($(GDB_MUL_EXITS),)
	GDB = gdb-multiarch
else
	GDB = gdb
endif

# 完整构建
all: prev build-user copy-user build copy
rv: prev-rv build-user-rv copy-user-rv build-rv copy-rv
la: prev-la build-user-la copy-user-la build-la copy-la

prev: prev-rv prev-la
prev-rv: 
	cd os && $(MAKE) env-la
prev-la: 
	cd os && $(MAKE) env-la

build: build-rv build-la
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

build-user: build-user-rv build-user-la
build-user-rv:
	cd user && $(MAKE) build ARCH=riscv64
build-user-la:
	cd user && $(MAKE) build ARCH=loongarch64

copy: copy-rv  copy-user-rv copy-la  copy-user-la
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

copy-user: copy-user-rv copy-user-la
copy-user-rv:
	cd user && find target/riscv64gc-unknown-none-elf/release/ -maxdepth 1 -name 'initproc*' ! -name '*.*' -exec cp -f {} ../os/src/arch/riscv/ \;
copy-user-la:
	cd user && find target/loongarch64-unknown-none/release/ -maxdepth 1 -name 'initproc*' ! -name '*.*' -exec cp -f {} ../os/src/arch/la/ \;

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

# debug-rv: MODE = debug
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

# Split the raw LoongArch disk image for U-Boot TFTP, then overwrite SATA LBA 0.
# SERIAL_PORT is deliberately required so an unintended serial adapter is not used.
sata:
	@test -n "$(SERIAL_PORT)" || { echo "SERIAL_PORT is required, e.g. make sata SERIAL_PORT=/dev/ttyUSB0"; exit 2; }
	@test -f "$(SATA_IMAGE)" || { echo "missing SATA_IMAGE: $(SATA_IMAGE)"; exit 2; }
	mkdir -p "$(TFTP_ROOT)"
	rm -f "$(TFTP_ROOT)/$(notdir $(SATA_IMAGE)).part-"*
	split -b 1G -d -a 3 "$(SATA_IMAGE)" "$(TFTP_ROOT)/$(notdir $(SATA_IMAGE)).part-"
	python3 toolkits/2k1000-board-formmater/auto-formmat.py \
		--serial "$(SERIAL_PORT)" --baud "$(SERIAL_BAUD)" \
		--board-ip "$(BOARD_IP)" $(if $(SERVER_IP),--server-ip "$(SERVER_IP)") \
		--tftp-root "$(TFTP_ROOT)" --image-name "$(notdir $(SATA_IMAGE))" \
		--tftp-timeout "$(TFTP_TIMEOUT)" --write-timeout "$(SATA_WRITE_TIMEOUT)" \
		--verify-timeout "$(SATA_VERIFY_TIMEOUT)" --yes

# Verify existing SATA contents against the already generated TFTP image parts.
# This target only reads SATA; it never runs `scsi write`.
sata-verify:
	@test -n "$(SERIAL_PORT)" || { echo "SERIAL_PORT is required, e.g. make sata-verify SERIAL_PORT=/dev/ttyUSB0"; exit 2; }
	python3 toolkits/2k1000-board-formmater/auto-formmat.py \
		--serial "$(SERIAL_PORT)" --baud "$(SERIAL_BAUD)" \
		--tftp-root "$(TFTP_ROOT)" --image-name "$(notdir $(SATA_IMAGE))" \
		--verify-timeout "$(SATA_VERIFY_TIMEOUT)" --verify-only
