all: build

build-rv:
	cd os && make build

build-la:
	cd os && make build-la

copy-rv:
	cd os && cp target/riscv64gc-unknown-none-elf/release/os ../kernel-rv

copy-la:
	cd os && cp target/loongarch64-unknown-none/release/os ../kernel-la

copy: copy-rv copy-la

build: build-rv build-la copy

test-rv: build-rv copy-rv
	@rm -f kernel_output.log
	@qemu-system-riscv64 -machine virt \
	-kernel kernel-rv \
	-m 1G -nographic -smp 1 \
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
	-smp 1 \
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
	-m 1G -nographic -smp 1 \
	-bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-no-shutdown \
	-device virtio-net-device,netdev=net \
	-netdev user,id=net \
	-rtc base=utc \
	-s -S | tee kernel_output.log

GDB_ELF := /root/rcore/_2026os/os/target/riscv64gc-unknown-none-elf/release/os

gdb:
	riscv64-unknown-elf-gdb \
		-ex 'file $(GDB_ELF)' \
		-ex 'set arch riscv:rv64' \
		-ex 'target remote localhost:1234' 
