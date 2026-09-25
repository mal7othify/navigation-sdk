#!/usr/bin/env bash
# Cross-compiles navcore-ffi for Android (arm64-v8a, armeabi-v7a, x86_64) into
# the library module's jniLibs, then regenerates the Kotlin bindings.
#
# Requires: cargo-ndk (`cargo install cargo-ndk`), the Android rustup targets,
# and an NDK under $ANDROID_HOME/ndk (or $ANDROID_NDK_HOME).
set -euo pipefail
cd "$(dirname "$0")/.."

: "${ANDROID_HOME:=${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
export ANDROID_HOME
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
fi
export ANDROID_NDK_HOME
echo "==> NDK: $ANDROID_NDK_HOME"

JNI_OUT=android/navsdk/src/main/jniLibs
MIN_SDK=24

echo "==> cargo ndk → $JNI_OUT"
cargo ndk --platform "$MIN_SDK" \
  -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o "$JNI_OUT" \
  build --release -p navcore-ffi

echo "==> bindings"
scripts/gen-bindings.sh

echo "==> jniLibs:"
find "$JNI_OUT" -name '*.so' -exec ls -la {} \;
