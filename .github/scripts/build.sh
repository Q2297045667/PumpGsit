#!/usr/bin/env bash
# Build the configured guest, type-check its Pumpkin/WASI ABI and stage both downloads.
set -euo pipefail
# shellcheck source=/dev/null
source .github/plugin.env

target=wasm32-wasip3
metadata=$(cargo metadata --locked --no-deps --format-version 1)
target_dir=$(jq -er '.target_directory' <<< "$metadata")
library=$(jq -er --arg pkg "$CARGO_PACKAGE" '
    [.packages[] | select(.name == $pkg) | .targets[] | select(.crate_types | index("cdylib")) | .name]
    | if length == 1 then .[0] else error("Expected one cdylib target for Cargo package \($pkg)") end
' <<< "$metadata")
component="$target_dir/$target/release/$library.wasm"
cargo fmt --all -- --check
cargo clippy --release --locked --package "$CARGO_PACKAGE" --lib --target "$target" -- -D warnings
cargo build --release --locked --package "$CARGO_PACKAGE" --lib --target "$target"
bash .github/scripts/check-abi.sh "$component"
wasm-tools component wit "$component" > "$target_dir/$PLUGIN_NAME.component.wit"

# Prepare the complete bundle before replacing the previous build's downloads.
stage=$(mktemp -d "$target_dir/.plugin-build.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/assets"
cp -- "$component" "$stage/assets/$PLUGIN_NAME.wasm"
(cd "$stage/assets" && sha256sum -- "$PLUGIN_NAME.wasm" > SHA256SUMS)
rm -rf -- "$target_dir/release-assets"
mv -T -- "$stage/assets" "$target_dir/release-assets"
