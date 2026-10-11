#!/usr/bin/env bash
# Phase 14: static size and code-volume comparison of the two approaches.
# Prints a Markdown table. Needs the Android libraries of both approaches built
# (scripts/build-android.sh, scripts/build-boltffi-android.sh) and, for the
# iOS rows, both XCFrameworks (scripts/build-ios.sh, scripts/build-boltffi-ios.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

: "${ANDROID_HOME:=${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
NDK="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)}"
STRIP="$(find "$NDK/toolchains/llvm/prebuilt" -name llvm-strip 2>/dev/null | head -1 || true)"

kb() { echo $(( $(stat -f %z "$1" 2>/dev/null || stat -c %s "$1") / 1024 )); }
stripped_kb() { # <lib>
  if [[ -n "$STRIP" ]]; then
    local tmp; tmp=$(mktemp)
    "$STRIP" --strip-all -o "$tmp" "$1" && kb "$tmp" && rm -f "$tmp"
  else
    echo "n/a"
  fi
}
dir_kb() { du -sk "$1" 2>/dev/null | cut -f1 || echo "n/a"; }
loc() { wc -l "$@" 2>/dev/null | tail -1 | awk '{print $1}'; }

echo "| Metric | A: Pull (UniFFI) | B: Push (BoltFFI) |"
echo "|---|---|---|"
for abi in arm64-v8a armeabi-v7a x86_64; do
  A=android/navsdk/src/main/jniLibs/$abi/libnavcore_ffi.so
  B=android/navsdk-push/src/main/jniLibs/$abi/libnavcore-boltffi.so
  if [[ -f $A && -f $B ]]; then
    echo "| Android .so $abi, as built | $(kb "$A") KB | $(kb "$B") KB |"
    echo "| Android .so $abi, llvm-strip --strip-all | $(stripped_kb "$A") KB | $(stripped_kb "$B") KB |"
  fi
done
if [[ -d ios/NavSDK/NavCoreFFI.xcframework && -d ios/NavSDKPush/NavCorePush.xcframework ]]; then
  echo "| iOS XCFramework on disk (all slices) | $(dir_kb ios/NavSDK/NavCoreFFI.xcframework) KB | $(dir_kb ios/NavSDKPush/NavCorePush.xcframework) KB |"
fi
echo "| Extra Android runtime deps | JNA AAR (~1.5 MB) + androidx.annotation | none (plain JNI) |"
echo "| Generated Kotlin, lines | $(loc android/navsdk/src/main/kotlin/com/navsdk/core/navcore.kt) | $(loc android/navsdk-push/generated/com/navsdk/push/core/NavCorePush.kt) (+ $(loc android/navsdk-push/generated/jni/jni_glue.c) lines of C JNI glue) |"
echo "| Generated Swift, lines | $(loc ios/NavSDK/Sources/NavSDK/Generated/NavCore.swift) | $(loc ios/NavSDKPush/Sources/BoltFFI/NavcoreBoltffiBoltFFI.swift) |"
echo "| FFI crate (hand-written Rust), lines | $(loc crates/navcore-ffi/src/lib.rs) | $(loc crates/navcore-boltffi/src/lib.rs) |"
echo "| Kotlin wrapper, lines | $(loc android/navsdk/src/main/kotlin/com/navsdk/NavigationSession.kt) | $(loc android/navsdk-push/src/main/kotlin/com/navsdk/push/NavigationSessionPush.kt) |"
echo "| Swift wrapper, lines | $(loc ios/NavSDK/Sources/NavSDK/NavigationSession.swift) | $(loc ios/NavSDKPush/Sources/NavSDKPush/NavigationSessionPush.swift) |"
