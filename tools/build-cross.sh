#!/bin/sh
# Cross-builds the CableBox .deb for another architecture on an x86_64 Debian host.
#
#   tools/build-cross.sh arm64     # Raspberry Pi 4/5 and other arm64 boards
#   tools/build-cross.sh riscv64   # RISC-V boards (VisionFive 2, Milk-V, ...)
#
# Needs: gcc-<triplet> (gcc-aarch64-linux-gnu / gcc-riscv64-linux-gnu), cargo-deb, and
# `rustup target add <rust target>`. The sysroot is created by tools/cross-sysroot.sh on first use.
set -eu
cd "$(dirname "$0")/.."

ARCH=${1:?usage: $0 arm64|riscv64 [cargo-deb args...]}
shift
case "$ARCH" in
    arm64)   TARGET=aarch64-unknown-linux-gnu;   TRIPLET=aarch64-linux-gnu ;;
    riscv64) TARGET=riscv64gc-unknown-linux-gnu; TRIPLET=riscv64-linux-gnu ;;
    *) echo "unsupported architecture: $ARCH (arm64, riscv64)" >&2; exit 2 ;;
esac

command -v "$TRIPLET-gcc" >/dev/null || { echo "missing $TRIPLET-gcc: sudo apt install gcc-$TRIPLET" >&2; exit 1; }

ROOT=$(realpath -m "target/$ARCH-sysroot/root")
LIBDIR="$ROOT/usr/lib/$TRIPLET"
[ -e "$LIBDIR/libmpv.so" ] || tools/cross-sysroot.sh "$ARCH"

ENV_TARGET=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
export "CARGO_TARGET_${ENV_TARGET}_LINKER=$TRIPLET-gcc"
# Only direct dependencies are in the sysroot, so let their own deps resolve on the device.
export "CARGO_TARGET_${ENV_TARGET}_RUSTFLAGS=-L $LIBDIR -C link-arg=-Wl,-rpath-link,$LIBDIR -C link-arg=-Wl,--allow-shlib-undefined"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$ROOT"
export PKG_CONFIG_LIBDIR="$LIBDIR/pkgconfig:$ROOT/usr/share/pkgconfig"

exec cargo deb -p cablebox --target "$TARGET" --variant "$ARCH" "$@"
