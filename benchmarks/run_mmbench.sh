#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MODE=${1:-linux}
LOOPS=${2:-5000}
WORKERS=${WORKERS:-8}
PORT=${PORT:-18081}
SMP=${SMP:-8}
TIMEOUT=${TIMEOUT:-180}
QEMU=${QEMU:-qemu-system-riscv64}
NET_DEVICE=${NET_DEVICE:-}
EMBEDDED=${EMBEDDED:-0}

if [ "$MODE" = kernel ] && [ "$EMBEDDED" = 1 ]; then
	if [ "$LOOPS" != 5000 ] || [ "$WORKERS" != 8 ]; then
		echo "embedded kernel mode uses mmbench defaults: loops=5000 workers=8" >&2
		exit 2
	fi
	make -C "$ROOT/benchmarks" kernel-mmbench
else
	make -C "$ROOT/benchmarks" mmbench
fi

if [ "$MODE" = linux ]; then
	KERNEL=${KERNEL_IMAGE:-"$ROOT/alpine_linux/Image"}
	INITRD="$ROOT/alpine_linux/initramfs-lts"
	APPEND="console=ttyS0 rdinit=/bin/sh"
	BUSYBOX=/usr/bin/busybox
	USE_DISK=0
	[ -n "$NET_DEVICE" ] || NET_DEVICE=virtio-net-pci
else
	if [ "$MODE" != kernel ]; then
		echo "usage: $0 linux|kernel [loops]" >&2
		exit 2
	fi
	if [ "$EMBEDDED" = 1 ]; then
		KERNEL=${KERNEL_IMAGE:-"$ROOT/benchmarks/build/kernel-rv-mmbench"}
		USE_DISK=1
	else
		KERNEL=${KERNEL_IMAGE:-"$ROOT/kernel-rv"}
		USE_DISK=1
	fi
	BUSYBOX=/musl/busybox
	[ -n "$NET_DEVICE" ] || NET_DEVICE=virtio-net-device
fi

HTTP_LOG=${TMPDIR:-/tmp}/mmbench-http.$$.log
GUEST_LOG=${TMPDIR:-/tmp}/mmbench-${MODE}.$$.log
QMP_SOCKET=""
WATCHER_PID=""
HTTP_PID=""

if [ "$MODE" = linux ]; then
	python3 -m http.server "$PORT" --bind 0.0.0.0 \
		--directory "$ROOT/benchmarks/build" >"$HTTP_LOG" 2>&1 &
	HTTP_PID=$!
fi

cleanup() {
	[ -z "$HTTP_PID" ] || kill "$HTTP_PID" 2>/dev/null || true
	[ -z "$WATCHER_PID" ] || kill "$WATCHER_PID" 2>/dev/null || true
	[ -z "$QMP_SOCKET" ] || rm -f "$QMP_SOCKET"
}
trap cleanup EXIT INT TERM

if [ "$MODE" = kernel ]; then
	QMP_SOCKET=${TMPDIR:-/tmp}/mmbench-qmp.$$.sock
	rm -f "$QMP_SOCKET"
	(
		waited=0
		while [ ! -S "$QMP_SOCKET" ] && [ "$waited" -lt "$TIMEOUT" ]; do
			sleep 1
			waited=$((waited + 1))
		done
		while [ "$waited" -lt "$TIMEOUT" ]; do
			if [ -f "$GUEST_LOG" ] && rg -q 'MMBENCH_END' "$GUEST_LOG"; then
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
if [ "$MODE" = linux ]; then
	set -- "$@" -initrd "$INITRD" -append "$APPEND"
elif [ "$USE_DISK" -eq 1 ]; then
	set -- "$@" \
		-drive "file=$ROOT/sdcard-rv.img,if=none,format=raw,id=x0" \
		-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -snapshot
fi
set -- "$@" -netdev user,id=net0 -device "$NET_DEVICE",netdev=net0 -no-reboot
if [ -n "$QMP_SOCKET" ]; then
	set -- "$@" -qmp "unix:$QMP_SOCKET,server=on,wait=off"
fi

echo "mmbench mode=$MODE loops=$LOOPS workers=$WORKERS log=$GUEST_LOG"
if [ "$EMBEDDED" = 1 ]; then
	timeout "$TIMEOUT" "$@" 2>&1 | tee "$GUEST_LOG"
	echo "mmbench log: $GUEST_LOG"
	exit 0
fi
{
	sleep 6
	if [ "$MODE" = linux ]; then
		printf '%s mount -t sysfs sysfs /sys 2>/dev/null || true\n' "$BUSYBOX"
		printf '%s mount -t devtmpfs devtmpfs /dev 2>/dev/null || true\n' "$BUSYBOX"
		printf '/usr/sbin/modprobe virtio_pci 2>/dev/null || true\n'
		printf '/usr/sbin/modprobe virtio_net 2>/dev/null || true\n'
		printf '%s mdev -s 2>/dev/null || true\n' "$BUSYBOX"
		printf '%s ifconfig eth0 10.0.2.15 netmask 255.255.255.0 up\n' "$BUSYBOX"
		printf '%s route add default gw 10.0.2.2\n' "$BUSYBOX"
		printf '%s mkdir -p /tmp\n' "$BUSYBOX"
		printf '%s wget -q -O /tmp/mmbench http://10.0.2.2:%s/mmbench\n' "$BUSYBOX" "$PORT"
	else
		printf '%s mkdir -p /tmp\n' "$BUSYBOX"
		printf '%s stty -echo\n' "$BUSYBOX"
		printf '%s base64 -d <<'\''MMBENCH_BASE64_END'\'' | %s gzip -d -c > /tmp/mmbench\n' "$BUSYBOX" "$BUSYBOX"
		gzip -9 -c "$ROOT/benchmarks/build/mmbench" | base64 |
			perl -e '$| = 1; while (read STDIN, $buf, 76) { print $buf; select undef, undef, undef, 0.01; }'
		printf 'MMBENCH_BASE64_END\n'
		printf '%s stty echo\n' "$BUSYBOX"
	fi
	printf '%s chmod +x /tmp/mmbench\n' "$BUSYBOX"
	printf 'echo MMBENCH_GUEST_BEGIN\n'
	printf '/tmp/mmbench %s %s\n' "$LOOPS" "$WORKERS"
	printf 'echo MMBENCH_GUEST_END\n'
	if [ "$MODE" = linux ]; then
		printf '%s poweroff -f\n' "$BUSYBOX"
	fi
} | timeout "$TIMEOUT" "$@" 2>&1 | tee "$GUEST_LOG"

echo "mmbench log: $GUEST_LOG"
