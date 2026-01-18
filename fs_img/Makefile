ARCH ?= riscv64
TARGET_EXT4 := $(ARCH)_ext4.img
SRC_DIR := $(ARCH)

.PHONY: run clean

run: run_ext4

run_ext4: 
	@echo "Ext4 file system image created: $(TARGET_EXT4)"
	@if [ ! -d "$(SRC_DIR)" ]; then \
		echo "Error: Directory $(SRC_DIR) does not exist."; \
		exit 1; \
	fi
	@DIR_SIZE_KB=$$(du -sk "$(SRC_DIR)" | cut -f1); \
	 echo "Source directory size: $${DIR_SIZE_KB} KB"; \
	 IMG_SIZE_MB=$$(( $${DIR_SIZE_KB} * 120 / 100 / 1024 + 5 )); \
	 echo "TARGET_EXT4 image size: $${IMG_SIZE_MB} MB"; \
	 echo "Generating $(TARGET_EXT4) from $(SRC_DIR)..."; \
	 dd if=/dev/zero of=$(TARGET_EXT4) bs=1M count=$${IMG_SIZE_MB}; \
	 mkfs.ext4 -d $(SRC_DIR) -F $(TARGET_EXT4)

clean:
	rm -f $(TARGET_EXT4)
