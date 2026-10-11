#!/usr/bin/env bash
# Regenerates Kotlin and Swift bindings from the built navcore-ffi library
# (UniFFI "library mode"). Generated files are gitignored; never edit them.
#
#   scripts/gen-bindings.sh [path/to/libnavcore_ffi.{dylib,so}]
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  Darwin) EXT=dylib ;;
  *)      EXT=so ;;
esac
LIB="${1:-target/release/libnavcore_ffi.$EXT}"

# Always rebuild the host library: the bindings are generated from metadata
# embedded in it, so a stale dylib silently produces stale bindings. Cargo is
# incremental, so this is free when nothing changed.
if [[ $# -eq 0 ]]; then
  echo "==> building navcore-ffi (release)"
  cargo build -p navcore-ffi --release
elif [[ ! -f "$LIB" ]]; then
  echo "error: $LIB not found" >&2
  exit 1
fi

KOTLIN_OUT=android/navsdk/src/main/kotlin
SWIFT_SRC_OUT=ios/NavSDK/Sources/NavSDK/Generated
SWIFT_FFI_OUT=ios/NavSDK/Generated
mkdir -p "$KOTLIN_OUT" "$SWIFT_SRC_OUT" "$SWIFT_FFI_OUT"

echo "==> kotlin → $KOTLIN_OUT"
cargo run -q -p uniffi-bindgen -- generate \
  --library "$LIB" --language kotlin --out-dir "$KOTLIN_OUT" --no-format

echo "==> swift → $SWIFT_SRC_OUT (sources), $SWIFT_FFI_OUT (header + modulemap)"
cargo run -q -p uniffi-bindgen -- generate \
  --library "$LIB" --language swift --out-dir "$SWIFT_FFI_OUT" --no-format
# SwiftPM must not see the C header inside the Swift target, so only the
# .swift file lives under Sources/.
mv -f "$SWIFT_FFI_OUT"/*.swift "$SWIFT_SRC_OUT"/

echo "==> generated:"
find "$KOTLIN_OUT" "$SWIFT_SRC_OUT" "$SWIFT_FFI_OUT" -type f | sort
