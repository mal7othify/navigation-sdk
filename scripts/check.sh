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

echo "==> no unwrap/expect in the FFI crates (nothing may panic across the FFI boundary)"
if grep -rnE '\.(unwrap|expect)\(' crates/navcore-ffi/src crates/navcore-boltffi/src --include='*.rs' | grep -vE '\.unwrap_or(_else|_default)?\('; then
  echo "found unwrap/expect in an FFI crate" >&2
  exit 1
fi

echo "==> OK"
