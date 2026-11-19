#!/usr/bin/env bash
# Build and validate the Wasm component, then assemble the downloadable files.
set -euo pipefail

target=wasm32-wasip2
target_dir=${CARGO_TARGET_DIR:-target}
assets="$target_dir/release-assets"
cargo fmt --check
cargo clippy --release --locked --target "$target" -- -D warnings
cargo build --release --locked --target "$target"
component="$target_dir/$target/release/pumpgsit.wasm"
wasm-tools validate --features all "$component"
wit="$target_dir/pumpgsit.component.wit"
wasm-tools component wit "$component" > "$wit"
grep -F 'export pumpkin:plugin/metadata@0.2.0;' "$wit"
grep -F 'export pumpkin:plugin/gametest-callbacks@0.2.0;' "$wit"

# Rebuild the staging directory when a previous build used the same target directory.
rm -rf -- "$assets"
mkdir -p "$assets"
cp "$component" "$assets/"
(cd "$assets" && sha256sum pumpgsit.wasm > SHA256SUMS)
