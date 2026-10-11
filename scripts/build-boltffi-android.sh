#!/usr/bin/env bash
# Approach B (push): cross-compiles navcore-boltffi for Android, compiles the
# JNI glue, regenerates the Kotlin bindings and builds the desktop JNI library
# used by the JVM unit tests. Outputs (all gitignored):
#   android/navsdk-push/generated/            Kotlin bindings + C glue
#   android/navsdk-push/src/main/jniLibs/     libnavcore-boltffi.so per ABI
#   android/navsdk-push/boltffi/desktopJniLibs/<os-arch>/libnavcore_boltffi_jni.*
#
# Requires: `cargo install boltffi_cli`, the Android rustup targets and an NDK
# (`boltffi check --android` lists what is missing).
set -euo pipefail
cd "$(dirname "$0")/../crates/navcore-boltffi"

echo "==> boltffi pack android --release"
boltffi pack android --release "$@"

echo "==> jniLibs:"
find ../../android/navsdk-push/src/main/jniLibs -name '*.so' -exec ls -la {} \;
