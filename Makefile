all: build-rv build-la

build-rv:
	cd os && make build

build-la:
	cd os && make build-la