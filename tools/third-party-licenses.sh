#!/bin/sh
# Regenerates packaging/THIRD-PARTY-LICENSES (shipped in the .deb) from the crates the
# package is built with. Run before each release; needs `cargo install cargo-about --features cli`.
set -eu
cd "$(dirname "$0")/.."
cargo about generate --fail --no-default-features --features kms,winit \
    -m crates/app/Cargo.toml -o packaging/THIRD-PARTY-LICENSES about.hbs
echo "wrote packaging/THIRD-PARTY-LICENSES ($(wc -l < packaging/THIRD-PARTY-LICENSES) lines)"
