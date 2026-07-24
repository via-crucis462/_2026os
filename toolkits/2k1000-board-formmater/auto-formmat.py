#!/usr/bin/env python3
#! 代码描述思路后用gpt生成
"""Write a raw disk image to the 2K1000's only SATA disk through U-Boot and TFTP."""

import argparse
import os
import re
import select
import socket
import sys
import termios
import time
import zlib
from pathlib import Path


SECTOR_SIZE = 512
MAX_PART_SIZE = 1 << 30
# This board's U-Boot exposes its only AHCI/SATA disk as SCSI device 0.
SCSI_DEVICE = 0
# Reported by `scsi info` for the fixed TS32GMTS400 disk: 62533296 x 512 B.
SATA_DISK_SECTORS = 62_533_296
# This is in DDR bank 1.  A 1 GiB transfer ends at 0x90000000d8000000,
# below the 0x9000000100000000 end of that bank.
LOAD_ADDRESS = 0x9000000098000000
PROMPT = b"=>"
ERROR_MARKERS = (
    b"TFTP error",
    b"SATA device not available",
    b"** Bad",
    b"Unknown command",
)


class SerialPort:
    """Minimal POSIX serial port implementation; avoids a pyserial dependency."""

    def __init__(self, path: str, baud: int) -> None:
        try:
            self.speed = getattr(termios, f"B{baud}")
        except AttributeError as error:
            raise ValueError(f"unsupported baud rate: {baud}") from error
        self.path = path
        self.fd: int | None = None

    def __enter__(self) -> "SerialPort":
        self.fd = os.open(self.path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        attrs = termios.tcgetattr(self.fd)
        attrs[0] = 0
        attrs[1] = 0
        attrs[2] &= ~(termios.PARENB | termios.CSTOPB | termios.CSIZE)
        attrs[2] |= termios.CS8 | termios.CLOCAL | termios.CREAD
        if hasattr(termios, "CRTSCTS"):
            attrs[2] &= ~termios.CRTSCTS
        attrs[3] = 0
        attrs[4] = self.speed
        attrs[5] = self.speed
        termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def reset_input_buffer(self) -> None:
        assert self.fd is not None
        termios.tcflush(self.fd, termios.TCIFLUSH)

    def write(self, data: bytes) -> None:
        assert self.fd is not None
        offset = 0
        while offset < len(data):
            _, writable, _ = select.select([], [self.fd], [], 10)
            if not writable:
                raise TimeoutError("serial write timed out")
            offset += os.write(self.fd, data[offset:])

    def flush(self) -> None:
        assert self.fd is not None
        termios.tcdrain(self.fd)

    def read(self, size: int, timeout: float) -> bytes:
        assert self.fd is not None
        readable, _, _ = select.select([self.fd], [], [], timeout)
        return os.read(self.fd, size) if readable else b""


def local_ip_for(board_ip: str) -> str:
    """Select the local address used by the route to the board, without traffic."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.connect((board_ip, 9))
        return sock.getsockname()[0]


def parts_in(tftp_root: Path, image_name: str) -> list[Path]:
    prefix = f"{image_name}.part-"
    parts = sorted(path for path in tftp_root.iterdir()
                   if path.is_file() and path.name.startswith(prefix))
    if not parts:
        raise ValueError(f"no parts named {prefix}NNN in {tftp_root}")

    expected = [f"{prefix}{index:03d}" for index in range(len(parts))]
    actual = [part.name for part in parts]
    if actual != expected:
        raise ValueError(f"non-contiguous part names: {', '.join(actual)}")
    for part in parts:
        size = part.stat().st_size
        if size == 0 or size > MAX_PART_SIZE or size % SECTOR_SIZE:
            raise ValueError(f"{part} must be 1..1GiB and a multiple of {SECTOR_SIZE} bytes")
    return parts


def crc32_file(path: Path) -> int:
    checksum = 0
    with path.open("rb") as image:
        while chunk := image.read(1024 * 1024):
            checksum = zlib.crc32(chunk, checksum)
    return checksum & 0xFFFFFFFF


def crc32_from_output(output: bytes) -> int:
    match = re.search(rb"==>\s*([0-9a-fA-F]{8})", output)
    if not match:
        raise RuntimeError("could not parse CRC32 output from U-Boot")
    return int(match.group(1), 16)


def wait_for_prompt(ser: SerialPort, timeout: float) -> bytes:
    deadline = time.monotonic() + timeout
    output = bytearray()
    while time.monotonic() < deadline:
        chunk = ser.read(4096, min(0.2, deadline - time.monotonic()))
        if chunk:
            output.extend(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            if PROMPT in output:
                return bytes(output)
    raise TimeoutError(f"U-Boot prompt not received within {timeout:g} seconds")


def run_command(ser: SerialPort, command: str, timeout: float) -> bytes:
    print(f"\n>>> {command}")
    ser.write(command.encode("ascii") + b"\r")
    ser.flush()
    output = wait_for_prompt(ser, timeout)
    if any(marker in output for marker in ERROR_MARKERS):
        raise RuntimeError(f"U-Boot reported an error while running: {command}")
    return output


def verify_parts(ser: SerialPort, parts: list[Path], timeout: float) -> None:
    """Read every image part from SATA and compare U-Boot CRC32 with the host file."""
    print("\nVerifying SATA contents with CRC32...")
    lba = 0
    for index, part in enumerate(parts):
        sectors = part.stat().st_size // SECTOR_SIZE
        expected = crc32_file(part)
        print(f"\nverify {index + 1}/{len(parts)}: {part.name}, LBA {lba}")
        run_command(ser, f"scsi read {LOAD_ADDRESS:#x} {lba:#x} {sectors:#x}", timeout)
        actual = crc32_from_output(
            run_command(ser, f"crc32 {LOAD_ADDRESS:#x} {part.stat().st_size:#x}", timeout)
        )
        if actual != expected:
            raise RuntimeError(
                f"CRC32 mismatch for {part.name}: SATA {actual:08x}, host {expected:08x}"
            )
        print(f"CRC32 OK: {actual:08x}")
        lba += sectors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True, help="serial device, e.g. /dev/ttyUSB0")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--board-ip", default="192.168.1.20",
                        help="board IP used to select the host interface")
    parser.add_argument("--server-ip", help="override the detected local TFTP server IP")
    parser.add_argument("--tftp-root", type=Path, required=True)
    parser.add_argument("--image-name", default="sdcard-la.img")
    parser.add_argument("--command-timeout", type=float, default=30)
    parser.add_argument("--tftp-timeout", type=float, default=900)
    parser.add_argument("--write-timeout", type=float, default=900,
                        help="maximum time to wait for each SATA write to finish")
    parser.add_argument("--verify-timeout", type=float, default=900,
                        help="maximum time to wait for each SATA read or CRC32 calculation")
    parser.add_argument("--verify-only", action="store_true",
                        help="verify SATA against local image parts without writing SATA")
    parser.add_argument("--skip-verify", action="store_true",
                        help="do not read back and CRC32-check the data after writing")
    parser.add_argument("--yes", action="store_true", help="confirm overwriting SATA from LBA 0")
    args = parser.parse_args()

    parts = parts_in(args.tftp_root, args.image_name)
    total_bytes = sum(part.stat().st_size for part in parts)
    total_sectors = total_bytes // SECTOR_SIZE
    if total_sectors > SATA_DISK_SECTORS:
        parser.error(
            f"image needs {total_sectors} sectors, exceeding the fixed SATA disk "
            f"capacity of {SATA_DISK_SECTORS} sectors"
        )
    if args.verify_only and args.skip_verify:
        parser.error("--verify-only and --skip-verify cannot be used together")
    if not args.verify_only and not args.yes:
        parser.error("this overwrites SATA LBA 0; pass --yes after checking the target disk")

    print(f"image: {len(parts)} part(s), {total_bytes} bytes, SATA LBA 0..{total_sectors - 1}")
    print(f"target: fixed SATA disk exposed as SCSI device {SCSI_DEVICE}")
    server_ip = None if args.verify_only else args.server_ip or local_ip_for(args.board_ip)
    if server_ip:
        print(f"server IP: {server_ip}")
    with SerialPort(args.serial, args.baud) as ser:
        ser.reset_input_buffer()
        ser.write(b"\r")
        ser.flush()
        wait_for_prompt(ser, args.command_timeout)
        run_command(ser, "scsi scan", args.command_timeout)
        run_command(ser, f"scsi device {SCSI_DEVICE}", args.command_timeout)

        if not args.verify_only:
            run_command(ser, f"setenv serverip {server_ip}", args.command_timeout)
            lba = 0
            for index, part in enumerate(parts):
                sectors = part.stat().st_size // SECTOR_SIZE
                print(f"\npart {index + 1}/{len(parts)}: {part.name}, LBA {lba}, {sectors} sectors")
                run_command(ser, f"tftp {LOAD_ADDRESS:#x} {part.name}", args.tftp_timeout)
                run_command(ser, f"scsi write {LOAD_ADDRESS:#x} {lba:#x} {sectors:#x}", args.write_timeout)
                lba += sectors
            print("\nSATA image write completed.")
        if not args.skip_verify:
            verify_parts(ser, parts, args.verify_timeout)
    print("\nSATA image verification completed.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, TimeoutError) as error:
        sys.exit(f"error: {error}")
