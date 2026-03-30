MODE ?= debug
RV_SMP ?= 1
LA_SMP ?= 4
RV_GDB_PORT ?= 1234
LA_GDB_PORT ?= 1235
RV_ELF ?= os/target/riscv64gc-unknown-none-elf/$(MODE)/os
LA_ELF ?= os/target/loongarch64-unknown-none/$(MODE)/os

all: build

build-rv:
	cd os && make build MODE=$(MODE)

build-la:
	cd os && make build-la MODE=$(MODE)

copy-rv:
	cd os && cp target/riscv64gc-unknown-none-elf/$(MODE)/os ../kernel-rv

copy-la:
	cd os && cp target/loongarch64-unknown-none/$(MODE)/os ../kernel-la

copy: copy-rv copy-la

build: build-rv build-la copy

test-rv: build-rv copy-rv
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

test-la: build-la copy-la
	@rm -f kernel_output.log
	@qemu-system-loongarch64 \
	-kernel kernel-la \
	-m 1G -nographic \
	-smp $(LA_SMP) \
	-drive file=sdcard-la.img,if=none,format=raw,id=x0 \
	-device virtio-blk-pci,drive=x0 \
	-no-reboot \
	-device virtio-net-pci,netdev=net0 \
	-netdev user,id=net0 \
	-rtc base=utc \
	| tee kernel_output.log

debug-rv: build-rv copy-rv
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
	-S -gdb tcp::$(RV_GDB_PORT) \
	| tee kernel_output.log

debug-la: build-la copy-la
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
	| tee kernel_output.log

gdb-rv:
	@gdb-multiarch $(RV_ELF) \
	-ex "set confirm off" \
	-ex "set pagination off" \
	-ex "set print thread-events off" \
	-ex "set scheduler-locking off" \
	-ex "set schedule-multiple on" \
	-ex "target extended-remote :$(RV_GDB_PORT)" \
	-ex "info threads"

gdb-la:
	@gdb-multiarch $(LA_ELF) \
	-ex "set confirm off" \
	-ex "set pagination off" \
	-ex "set print thread-events off" \
	-ex "set schedule-multiple off" \
	-ex "set scheduler-locking on" \
	-ex "set tdesc filename tools/la-gdb/loongarch64-fpu-lsx-lasx-lbt.xml" \
	-ex "target remote :$(LA_GDB_PORT)" \
	-ex "info threads"
