#!/usr/bin/env python3
"""
Wrap a LoongArch64 kernel ELF (from Rust or C) into a uImage for U-Boot.

Usage:
  python3 build_uimage.py <kernel_elf> [output_uimage]

Example:
  python3 build_uimage.py ../kernel-la-2k1000 ../kernel-la-2k1000.uImage
"""

import struct, zlib, os, sys, time, subprocess

# ========== Config ==========
OBJCOPY = "rust-objcopy"   # works for both Rust & C ELFs
READELF = "readelf"        # system readelf, works with any ELF
# ============================

# uImage header constants
IH_MAGIC          = 0x27051956
IH_OS_LINUX       = 5
IH_ARCH_LOONGARCH = 0x1B
IH_TYPE_KERNEL    = 2
IH_COMP_NONE      = 0

IMAGE_NAME = b"Custom-LA-kernel\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def elf_get_entry_and_load(elf_path):
    """Parse ELF to get physical entry point and load address."""
    # Get entry VA
    out = subprocess.check_output([READELF, "-h", elf_path], text=True)
    for line in out.splitlines():
        if "入口点地址" in line or "Entry point" in line:
            entry_va = int(line.strip().split()[-1], 16)
            break
    # Get LOAD VA (3rd field: file_off VA PA ...)
    out = subprocess.check_output([READELF, "-l", elf_path], text=True)
    for line in out.splitlines():
        if "LOAD" in line and "0x" in line:
            parts = line.split()
            load_va = int(parts[2], 16)
            break

    phys_load = load_va & 0xFFFFFFFFFFFF
    phys_entry = (phys_load + (entry_va - load_va)) & 0xFFFFFFFFFFFF
    return phys_load, phys_entry


def build_uimage(elf_path, output_path):
    phys_load, phys_entry = elf_get_entry_and_load(elf_path)
    print(f"ELF:  load VA → phys 0x{phys_load:08X}")
    print(f"ELF:  entry VA → phys 0x{phys_entry:08X}")

    # ELF -> raw binary
    print("[1/2] Converting ELF to raw binary...")
    subprocess.run([OBJCOPY, "-O", "binary", elf_path, output_path + ".bin"], check=True)
    bin_size = os.path.getsize(output_path + ".bin")
    print(f"  Binary size: {bin_size} bytes ({bin_size/1024/1024:.2f} MiB)")

    # Build uImage
    print("[2/2] Building uncompressed uImage...")
    with open(output_path + ".bin", "rb") as f:
        data = f.read()

    hdr = bytearray(64)
    struct.pack_into(">I", hdr, 0, IH_MAGIC)
    struct.pack_into(">I", hdr, 4, 0)
    struct.pack_into(">I", hdr, 8, int(time.time()))
    struct.pack_into(">I", hdr, 12, len(data))
    struct.pack_into(">I", hdr, 16, phys_load)
    struct.pack_into(">I", hdr, 20, phys_entry)
    struct.pack_into(">I", hdr, 24, crc32(data))
    hdr[28] = IH_OS_LINUX
    hdr[29] = IH_ARCH_LOONGARCH
    hdr[30] = IH_TYPE_KERNEL
    hdr[31] = IH_COMP_NONE
    name_bytes = IMAGE_NAME[:32].ljust(32, b'\x00')
    hdr[32:64] = name_bytes
    struct.pack_into(">I", hdr, 4, crc32(bytes(hdr)))

    with open(output_path, "wb") as f:
        f.write(bytes(hdr))
        f.write(data)

    os.remove(output_path + ".bin")
    print(f"\n  uImage: {output_path}")
    print(f"  Load: 0x{phys_load:08X}  Entry: 0x{phys_entry:08X}")
    print(f"  Size: {len(data)} bytes, CRC OK")
    print("Done!")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <kernel_elf> [output]")
        print(f"Example: {sys.argv[0]} ../kernel-la-2k1000")
        sys.exit(1)
    elf = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else elf + ".uImage"
    build_uimage(elf, out)