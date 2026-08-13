#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MODE=${1:-linux}
ITERS=${2:-2000}
PORT=${PORT:-18080}
SMP=${SMP:-8}
TIMEOUT=${TIMEOUT:-180}
QEMU=${QEMU:-qemu-system-riscv64}
NET_DEVICE=${NET_DEVICE:-}
EMBEDDED=${EMBEDDED:-0}

if [ "$MODE" = kernel ] && [ "$EMBEDDED" = 1 ]; then
	make -C "$ROOT/benchmarks" kernel-waitbench
else
	make -C "$ROOT/benchmarks" waitbench
fi

case "$MODE" in
linux)
	KERNEL=${KERNEL_IMAGE:-"$ROOT/alpine_linux/Image"}
	INITRD="$ROOT/alpine_linux/initramfs-lts"
	APPEND="console=ttyS0 rdinit=/bin/sh"
	USE_DISK=0
	BUSYBOX=/usr/bin/busybox
	[ -n "$NET_DEVICE" ] || NET_DEVICE=virtio-net-pci
	;;
kernel)
	if [ "$EMBEDDED" = 1 ]; then
		KERNEL=${KERNEL_IMAGE:-"$ROOT/benchmarks/build/kernel-rv-waitbench"}
	else
		KERNEL=${KERNEL_IMAGE:-"$ROOT/kernel-rv"}
	fi
	INITRD=""
	APPEND=""
	USE_DISK=1
	BUSYBOX=/musl/busybox
	[ -n "$NET_DEVICE" ] || NET_DEVICE=virtio-net-device
	;;
*)
	echo "usage: $0 linux|kernel [iterations]" >&2
	exit 2
	;;
esac

if [ "$EMBEDDED" = 1 ] && [ "$MODE" != kernel ]; then
	echo "EMBEDDED=1 is supported only in kernel mode" >&2
	exit 2
fi
if [ "$EMBEDDED" = 1 ] && [ "$ITERS" != 2000 ]; then
	echo "embedded kernel mode currently uses the built-in 2000 iterations" >&2
	exit 2
fi

HTTP_LOG=${TMPDIR:-/tmp}/waitbench-http.$$.log
GUEST_LOG=${TMPDIR:-/tmp}/waitbench-${MODE}.$$.log
HTTP_PID=""
QMP_SOCKET=""
WATCHER_PID=""
if [ "$MODE" = linux ]; then
	python3 -m http.server "$PORT" --bind 0.0.0.0 --directory "$ROOT/benchmarks/build" >"$HTTP_LOG" 2>&1 &
	HTTP_PID=$!
fi
cleanup() {
	[ -z "$HTTP_PID" ] || kill "$HTTP_PID" 2>/dev/null || true
	[ -z "$WATCHER_PID" ] || kill "$WATCHER_PID" 2>/dev/null || true
	[ -z "$QMP_SOCKET" ] || rm -f "$QMP_SOCKET"
}
trap cleanup EXIT INT TERM

if [ "$MODE" = kernel ]; then
	QMP_SOCKET=${TMPDIR:-/tmp}/waitbench-qmp.$$.sock
	rm -f "$QMP_SOCKET"
	(
		waited=0
		while [ ! -S "$QMP_SOCKET" ] && [ "$waited" -lt "$TIMEOUT" ]; do
			sleep 1
			waited=$((waited + 1))
		done
		while [ "$waited" -lt "$TIMEOUT" ]; do
			if [ -f "$GUEST_LOG" ] && rg -q 'WAITBENCH_DONE' "$GUEST_LOG"; then
				printf '%s\n%s\n' '{"execute":"qmp_capabilities"}' '{"execute":"quit"}' |
					socat - "UNIX-CONNECT:$QMP_SOCKET" >/dev/null 2>&1 || true
				exit 0
			fi
			sleep 1
			waited=$((waited + 1))
		done
	) &
	WATCHER_PID=$!
fi

set -- "$QEMU" -machine virt -m 2G -smp "$SMP" -nographic -bios default -kernel "$KERNEL"
if [ -n "$INITRD" ]; then
	set -- "$@" -initrd "$INITRD"
fi
if [ -n "$APPEND" ]; then
	set -- "$@" -append "$APPEND"
fi
if [ "$USE_DISK" -eq 1 ]; then
	set -- "$@" \
		-drive "file=$ROOT/sdcard-rv.img,if=none,format=raw,id=x0" \
		-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -snapshot
fi
set -- "$@" -netdev user,id=net0 -device "$NET_DEVICE",netdev=net0 -no-reboot
if [ -n "$QMP_SOCKET" ]; then
	set -- "$@" -qmp "unix:$QMP_SOCKET,server=on,wait=off"
fi

echo "waitbench mode=$MODE iters=$ITERS log=$GUEST_LOG"
if [ "$EMBEDDED" = 1 ]; then
	timeout "$TIMEOUT" "$@" 2>&1 | tee "$GUEST_LOG"
else
	{
	sleep 6
	if [ "$MODE" = linux ]; then
		printf '%s mount -t sysfs sysfs /sys 2>/dev/null || true\n' "$BUSYBOX"
		printf '%s mount -t devtmpfs devtmpfs /dev 2>/dev/null || true\n' "$BUSYBOX"
		printf '/usr/sbin/modprobe virtio_pci 2>/dev/null || true\n'
		printf '/usr/sbin/modprobe virtio_net 2>/dev/null || true\n'
		printf '%s mdev -s 2>/dev/null || true\n' "$BUSYBOX"
		printf '%s mkdir -p /tmp\n' "$BUSYBOX"
		printf '%s ifconfig eth0 10.0.2.15 netmask 255.255.255.0 up\n' "$BUSYBOX"
		printf '%s route add default gw 10.0.2.2\n' "$BUSYBOX"
		printf '%s wget -q -O /tmp/waitbench http://10.0.2.2:%s/waitbench\n' "$BUSYBOX" "$PORT"
	else
		printf '%s mkdir -p /tmp\n' "$BUSYBOX"
		printf '%s stty -echo\n' "$BUSYBOX"
		printf '%s base64 -d <<'\''WAITBENCH_BASE64_END'\'' | %s gzip -d -c > /tmp/waitbench\n' "$BUSYBOX" "$BUSYBOX"
		gzip -9 -c "$ROOT/benchmarks/build/waitbench" | base64 |
			perl -e '$| = 1; while (read STDIN, $buf, 76) { print $buf; select undef, undef, undef, 0.01; }'
		printf 'WAITBENCH_BASE64_END\n'
		printf '%s stty echo\n' "$BUSYBOX"
	fi
	printf '%s chmod +x /tmp/waitbench\n' "$BUSYBOX"
	printf 'echo WAITBENCH_GUEST_BEGIN\n'
	printf '/tmp/waitbench %s\n' "$ITERS"
	printf 'echo WAITBENCH_GUEST_END\n'
		printf '%s poweroff -f\n' "$BUSYBOX"
	} | timeout "$TIMEOUT" "$@" 2>&1 | tee "$GUEST_LOG"
fi

echo "waitbench log: $GUEST_LOG"
