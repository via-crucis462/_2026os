#!/bin/bash
# Wrapper to inject --sysroot pointing to local core/alloc source.
# Usage: RUSTC=./rustc-sysroot-wrap.sh cargo build ...
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CUSTOM_SYSROOT="$SCRIPT_DIR/../deps/rust/nightly-2026-01-01-x86_64-unknown-linux-gnu/custom-sysroot"
# Canonicalize to avoid cargo workspace path mismatch
CUSTOM_SYSROOT="$(cd "$CUSTOM_SYSROOT" 2>/dev/null && pwd || echo "$CUSTOM_SYSROOT")"
REAL_RUSTC="$(rustup which rustc 2>/dev/null || echo rustc)"
exec "$REAL_RUSTC" --sysroot="$CUSTOM_SYSROOT" "$@"
