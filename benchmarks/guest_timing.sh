#!/bin/sh
# Guest-side timing harness. Host timestamps on the serial log are authoritative;
# guest prints markers only.
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28
export CARGO_NET_OFFLINE=true

echo "TIMING_HARNESS_BEGIN"

echo "START_CARGO_VERSION"
cargo --version
echo "END_CARGO_VERSION"

echo "START_RUSTC_VERSION"
rustc --version
echo "END_RUSTC_VERSION"

for i in 1 2 3 4 5; do
  echo "START_CARGO_HELPTIMES_$i"
  cargo --help >/dev/null 2>&1
  echo "END_CARGO_HELPTIMES_$i"
done

for i in 1 2 3; do
  echo "START_MINIBUILD_$i"
  rm -rf /tmp/minibuild
  cargo new --vcs none /tmp/minibuild >/dev/null 2>&1
  ( cd /tmp/minibuild && cargo build >/dev/null 2>&1 )
  /tmp/minibuild/target/debug/minibuild
  echo "END_MINIBUILD_$i"
done

echo "TIMING_HARNESS_END"
sync
