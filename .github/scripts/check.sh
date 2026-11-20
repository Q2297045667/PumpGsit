#!/usr/bin/env bash
# Run repository checks and host-side Rust tests from the project root.
set -euo pipefail

cargo fmt --all -- --check
shfmt -d .github/scripts
shellcheck .github/scripts/*.sh
actionlint
typos . .github
cargo machete

# Cargo defaults to the plugin target; the unit tests use the host's filesystem.
host=$(rustc --print host-tuple)
# The library and test harness each expand the full WIT bindings. Build one at a time by default.
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}
cargo clippy --locked --all-targets --target "$host" -- -D warnings
cargo test --locked --all-targets --target "$host"
