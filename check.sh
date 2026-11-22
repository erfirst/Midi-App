#!/usr/bin/env bash
# Runs the same checks as CI.
set -eux

cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown -- -D warnings
cargo test
trunk build --release
