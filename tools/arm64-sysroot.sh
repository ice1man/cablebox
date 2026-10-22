#!/bin/sh
# Builds a minimal arm64 sysroot (Debian trixie) for cross-compiling CableBox without root.
#
# Downloads only the libraries the binary links against, verifies the archive signature
# (InRelease via debian-archive-keyring) and every hash, then unpacks with `dpkg -x`.
# glibc comes from the host's cross toolchain (gcc-aarch64-linux-gnu).
#
#   tools/arm64-sysroot.sh [OUT_DIR]      # default: target/arm64-sysroot
set -eu

SUITE=trixie
MIRROR=https://deb.debian.org/debian
KEYRING=/usr/share/keyrings/debian-archive-keyring.gpg
PACKAGES="libmpv2 libmpv-dev libinput10 libinput-dev libgbm1 libgbm-dev libudev1 libudev-dev \
libxkbcommon0 libxkbcommon-dev"

OUT=$(realpath -m "${1:-target/arm64-sysroot}")
DL="$OUT/download"
ROOT="$OUT/root"
rm -rf "$DL" "$ROOT"
mkdir -p "$DL" "$ROOT"

echo "fetching $SUITE release index"
curl -fsSL "$MIRROR/dists/$SUITE/InRelease" -o "$DL/InRelease"
gpgv --keyring "$KEYRING" --output "$DL/Release" "$DL/InRelease" 2>/dev/null \
    || { echo "InRelease signature check failed" >&2; exit 1; }

IDX=main/binary-arm64/Packages.xz
want=$(awk -v f="$IDX" '/^SHA256:/{s=1;next} /^[^ ]/{s=0} s && $3==f {print $1}' "$DL/Release")
[ -n "$want" ] || { echo "no hash for $IDX in Release" >&2; exit 1; }
curl -fsSL "$MIRROR/dists/$SUITE/$IDX" -o "$DL/Packages.xz"
echo "$want  $DL/Packages.xz" | sha256sum -c --quiet
xz -dc "$DL/Packages.xz" > "$DL/Packages"

for pkg in $PACKAGES; do
    line=$(awk -v p="$pkg" '
        /^Package: /{cur=$2} /^Filename: /{f=$2} /^SHA256: /{h=$2}
        /^$/{ if (cur==p) {print f, h; exit} cur=""; f=""; h="" }' "$DL/Packages")
    [ -n "$line" ] || { echo "package $pkg not found" >&2; exit 1; }
    set -- $line
    deb="$DL/$(basename "$1")"
    echo "  $pkg"
    curl -fsSL "$MIRROR/$1" -o "$deb"
    echo "$2  $deb" | sha256sum -c --quiet
    dpkg -x "$deb" "$ROOT"
done

# Absolute symlinks (e.g. libfoo.so -> /usr/lib/...) must point inside the sysroot.
find "$ROOT" -type l | while read -r l; do
    t=$(readlink "$l")
    case "$t" in /*) ln -sfn "$ROOT$t" "$l" ;; esac
done

echo "sysroot ready: $ROOT"
