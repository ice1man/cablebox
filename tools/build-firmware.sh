#!/bin/sh
# Builds the IR receiver firmware into target/firmware/ir1/, where the package picks it up.
# Needs `rustup target add thumbv6m-none-eabi`.
set -eu
cd "$(dirname "$0")/.."

(cd firmware/rp2040/bootloader && cargo build --release)
(cd firmware/rp2040 && cargo build --release)

ELF=thumbv6m-none-eabi/release
OUT=target/firmware/ir1
tools/firmware-image.py "firmware/rp2040/bootloader/target/$ELF/cablebox-ir-boot" \
    "firmware/rp2040/target/$ELF/cablebox-ir" "$OUT"
sed -n 's/^version = "\(.*\)"/\1/p' firmware/rp2040/Cargo.toml | head -1 > "$OUT/version"
echo "wrote $OUT for firmware $(cat "$OUT/version")"
