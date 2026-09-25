#!/usr/bin/env bash
# Runs the checks that must pass before every commit: fmt, clippy (-D warnings), tests.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo test"
cargo test --workspace --all-features

echo "==> OK"
