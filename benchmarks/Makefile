CC := riscv64-linux-gnu-gcc
CFLAGS ?= -O2 -Wall -Wextra -Werror
LDFLAGS ?= -static -s

BUILD_DIR ?= build
KERNEL_WAITBENCH := $(BUILD_DIR)/kernel-rv-waitbench
KERNEL_MMBENCH := $(BUILD_DIR)/kernel-rv-mmbench

.PHONY: all clean waitbench kernel-waitbench mmbench kernel-mmbench

all: waitbench

mmbench: $(BUILD_DIR)/mmbench

waitbench: $(BUILD_DIR)/waitbench

kernel-waitbench: $(KERNEL_WAITBENCH)

kernel-mmbench: $(KERNEL_MMBENCH)

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

$(BUILD_DIR)/waitbench: waitbench.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) -pthread $(LDFLAGS) -o $@ $<

$(BUILD_DIR)/mmbench: mmbench.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) -pthread $(LDFLAGS) -o $@ $<

$(KERNEL_WAITBENCH): $(BUILD_DIR)/waitbench ../user/src/bin/initproc_waitbench.rs ../os/build.rs ../os/src/process/init.rs
	cd ../user && cargo build --target riscv64gc-unknown-none-elf --release --bin initproc_waitbench
	cp ../user/target/riscv64gc-unknown-none-elf/release/initproc_waitbench ../os/src/arch/riscv/initproc_waitbench
	$(MAKE) -C ../os kernel MODE=release INIT=waitbench BOARD=virt
	cp ../os/target/riscv64gc-unknown-none-elf/release/os $@

$(KERNEL_MMBENCH): $(BUILD_DIR)/mmbench ../user/src/bin/initproc_mmbench.rs ../os/build.rs ../os/src/process/init.rs
	cd ../user && cargo build --target riscv64gc-unknown-none-elf --release --bin initproc_mmbench
	cp ../user/target/riscv64gc-unknown-none-elf/release/initproc_mmbench ../os/src/arch/riscv/initproc_mmbench
	$(MAKE) -C ../os kernel MODE=release INIT=mmbench BOARD=virt
	cp ../os/target/riscv64gc-unknown-none-elf/release/os $@

clean:
	rm -f $(BUILD_DIR)/waitbench $(KERNEL_WAITBENCH) \
		$(BUILD_DIR)/mmbench $(KERNEL_MMBENCH)
