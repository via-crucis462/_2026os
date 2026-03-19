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