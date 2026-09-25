#!/usr/bin/env bash
# Builds navcore-ffi as static libraries for iOS device + simulator, regenerates
# the Swift bindings, and assembles ios/NavSDK/NavCoreFFI.xcframework.
#
#   scripts/build-ios.sh            # device + simulator (arm64 + x86_64)
#   SKIP_DEVICE=1 scripts/build-ios.sh   # simulator only (faster for tests)
set -euo pipefail
cd "$(dirname "$0")/.."

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.0}"

SIM_TARGETS=(aarch64-apple-ios-sim x86_64-apple-ios)
DEVICE_TARGET=aarch64-apple-ios
LIB=libnavcore_ffi.a

echo "==> cargo build (simulator: ${SIM_TARGETS[*]})"
for t in "${SIM_TARGETS[@]}"; do
  cargo build --release -p navcore-ffi --target "$t"
done
if [[ -z "${SKIP_DEVICE:-}" ]]; then
  echo "==> cargo build (device: $DEVICE_TARGET)"
  cargo build --release -p navcore-ffi --target "$DEVICE_TARGET"
fi

echo "==> lipo simulator slices"
SIM_OUT=target/ios-simulator-universal/release
mkdir -p "$SIM_OUT"
lipo -create "${SIM_TARGETS[@]/#/target/}" -output "$SIM_OUT/$LIB" 2>/dev/null || \
lipo -create \
  "target/${SIM_TARGETS[0]}/release/$LIB" \
  "target/${SIM_TARGETS[1]}/release/$LIB" \
  -output "$SIM_OUT/$LIB"

echo "==> bindings"
scripts/gen-bindings.sh

echo "==> headers"
HEADERS=target/ios-headers
rm -rf "$HEADERS" && mkdir -p "$HEADERS"
cp ios/NavSDK/Generated/NavCoreFFI.h "$HEADERS/"
# xcframework headers need the canonical module map name.
cp ios/NavSDK/Generated/NavCoreFFI.modulemap "$HEADERS/module.modulemap"

echo "==> xcframework"
XCF=ios/NavSDK/NavCoreFFI.xcframework
rm -rf "$XCF"
ARGS=(-library "$SIM_OUT/$LIB" -headers "$HEADERS")
if [[ -z "${SKIP_DEVICE:-}" ]]; then
  ARGS+=(-library "target/$DEVICE_TARGET/release/$LIB" -headers "$HEADERS")
fi
xcodebuild -create-xcframework "${ARGS[@]}" -output "$XCF" | tail -1

echo "==> test fixtures"
FIX=ios/NavSDK/Tests/NavSDKTests/Fixtures
rm -rf "$FIX" && mkdir -p "$FIX"
cp fixtures/*.json "$FIX/"
DEMO_FIX=ios/Demo/Demo/Fixtures
rm -rf "$DEMO_FIX" && mkdir -p "$DEMO_FIX"
cp fixtures/*.json "$DEMO_FIX/"

echo "==> done: $XCF"
ls "$XCF"
