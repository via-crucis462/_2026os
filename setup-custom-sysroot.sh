#!/bin/bash
# 初始化/更新 custom-sysroot 的符号链接
# 每次在新机器上首次构建前运行一次即可

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CUSTOM="$SCRIPT_DIR/deps/rust/nightly-2026-01-01-x86_64-unknown-linux-gnu/custom-sysroot"
REAL_SYSROOT="$(rustc --print sysroot)"

echo "真实 sysroot: $REAL_SYSROOT"
echo "custom-sysroot: $CUSTOM"

# 删除已有文件/链接/目录，然后创建符号链接
link_or_skip() {
    local target="$1"
    local link="$2"
    local name="$(basename "$link")"

    if [ ! -e "$target" ]; then
        echo "SKIP: $name （目标 $target 不存在）"
        return
    fi

    if [ -L "$link" ]; then
        rm "$link"
    elif [ -d "$link" ]; then
        # 只删空目录，非空说明是本地修改，不碰
        if [ -z "$(ls -A "$link" 2>/dev/null)" ]; then
            rmdir "$link"
        else
            echo "SKIP: $name （非空目录，不覆盖）"
            return
        fi
    elif [ -f "$link" ]; then
        rm "$link"
    fi

    ln -s "$target" "$link"
    echo "LINK: $name -> $target"
}

# ------ 1. custom-sysroot 根目录 ------
echo ""
echo "=== custom-sysroot/ ==="
link_or_skip "$REAL_SYSROOT/bin" "$CUSTOM/bin"

# ------ 2. lib/rustlib/ 下的目标平台目录 ------
RUSTLIB="$CUSTOM/lib/rustlib"
REAL_RUSTLIB="$REAL_SYSROOT/lib/rustlib"
echo ""
echo "=== lib/rustlib/ ==="
link_or_skip "$REAL_RUSTLIB/etc"                        "$RUSTLIB/etc"
link_or_skip "$REAL_RUSTLIB/loongarch64-unknown-none"   "$RUSTLIB/loongarch64-unknown-none"
link_or_skip "$REAL_RUSTLIB/riscv64gc-unknown-none-elf" "$RUSTLIB/riscv64gc-unknown-none-elf"
link_or_skip "$REAL_RUSTLIB/x86_64-unknown-linux-gnu"   "$RUSTLIB/x86_64-unknown-linux-gnu"

# ------ 3. library/ 下的符号链接（除 core/alloc/compiler-builtins 外全部）------
LIB="$RUSTLIB/src/rust/library"
REAL_LIB="$REAL_RUSTLIB/src/rust/library"
echo ""
echo "=== library/ ==="

# 三个本地修改的目录，不动
KEEP="core alloc compiler-builtins"

for dir in "$REAL_LIB"/*/; do
    name="$(basename "$dir")"
    if echo "$KEEP" | grep -qw "$name"; then
        echo "KEEP: $name （本地修改，不链接）"
        continue
    fi
    link_or_skip "$REAL_LIB/$name" "$LIB/$name"
done

echo ""
echo "DONE: custom-sysroot 的符号链接已全部更新。"
