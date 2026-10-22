#!/bin/sh
# Cross-builds the arm64 (Raspberry Pi 4/5) .deb on an x86_64 Debian host.
#
# Needs: gcc-aarch64-linux-gnu, cargo-deb, `rustup target add aarch64-unknown-linux-gnu`.
# The sysroot is created by tools/arm64-sysroot.sh on first use.
set -eu
cd "$(dirname "$0")/.."

TARGET=aarch64-unknown-linux-gnu
ROOT=$(realpath -m target/arm64-sysroot/root)
LIBDIR="$ROOT/usr/lib/aarch64-linux-gnu"

[ -e "$LIBDIR/libmpv.so" ] || tools/arm64-sysroot.sh

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
# Only direct dependencies are in the sysroot, so let their own deps resolve on the device.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-L $LIBDIR -C link-arg=-Wl,-rpath-link,$LIBDIR -C link-arg=-Wl,--allow-shlib-undefined"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$ROOT"
export PKG_CONFIG_LIBDIR="$LIBDIR/pkgconfig:$ROOT/usr/share/pkgconfig"

exec cargo deb -p cablebox --target "$TARGET" --variant arm64 "$@"
