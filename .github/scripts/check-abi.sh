#!/usr/bin/env bash
# Type-check a built component against the Pumpkin guest world and check its WASI imports.
set -euo pipefail

component=${1:?pass the path to the built Wasm component}
wasm-tools validate --features all "$component"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
wasm-tools component wit "$component" --out-dir "$work"
actual="$work/component.wit"
for interface in filesystem/types sockets/types sockets/ip-name-lookup; do
    grep -E "^  import wasi:${interface}@0\\.3\\.[0-9]+;$" "$actual" > /dev/null || {
        echo "Missing required WASI 0.3 import: $interface" >&2
        exit 1
    }
done
if grep '^  import wasi:' "$actual" | grep -vE '@0\.3\.[0-9]+;$'; then
    echo 'The component imports a WASI interface outside 0.3.' >&2
    exit 1
fi

# Rust links WASI imports in addition to Pumpkin's world. Keep those imports but
# use the host's checked-in types for all Pumpkin interfaces and callbacks.
rm "$work/deps/plugin.wit"
cp -R wit "$work/deps/pumpkin"
{
    echo 'package pumpgsit:build;'
    echo 'world host {'
    echo '  include pumpkin:plugin/plugin@0.2.0;'
    grep '^  import wasi:' "$actual"
    echo '}'
} > "$work/host.wit.tmp"
mv "$work/host.wit.tmp" "$actual"
wasm-tools component targets --world host "$work" "$component"
