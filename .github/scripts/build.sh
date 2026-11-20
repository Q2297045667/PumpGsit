#!/usr/bin/env bash
# Build and validate the Wasm component, then assemble the downloadable files.
set -euo pipefail

target=wasm32-wasip3
target_dir=${CARGO_TARGET_DIR:-target}
assets="$target_dir/release-assets"
cargo fmt --all -- --check
cargo clippy --release --locked --target "$target" -- -D warnings
cargo build --release --locked --target "$target"
component="$target_dir/$target/release/pumpgsit.wasm"
bash .github/scripts/check-abi.sh "$component"
wasm-tools component wit "$component" > "$target_dir/PumpGsit.component.wit"

# Rebuild the staging directory when a previous build used the same target directory.
rm -rf -- "$assets"
mkdir -p "$assets"
cp "$component" "$assets/PumpGsit.wasm"
(cd "$assets" && sha256sum PumpGsit.wasm > SHA256SUMS)
