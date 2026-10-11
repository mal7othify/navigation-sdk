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

# Package each slice as a static *framework* rather than a bare .a + headers.
# Two bare-library XCFrameworks in one app both copy their `module.modulemap`
# into the same `include/` directory and Xcode fails with "Multiple commands
# produce … module.modulemap"; frameworks keep their module maps to themselves.
make_framework() { # <static lib> <output dir>
  local lib="$1" out="$2" fw="$2/NavCoreFFI.framework"
  rm -rf "$fw"
  mkdir -p "$fw/Headers" "$fw/Modules"
  cp "$lib" "$fw/NavCoreFFI"
  cp ios/NavSDK/Generated/NavCoreFFI.h "$fw/Headers/"
  cat > "$fw/Modules/module.modulemap" <<'MM'
framework module NavCoreFFI {
    umbrella header "NavCoreFFI.h"
    export *
    use "Darwin"
    use "_Builtin_stdbool"
    use "_Builtin_stdint"
}
MM
  cat > "$fw/Info.plist" <<'PL'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.navsdk.NavCoreFFI</string>
  <key>CFBundleName</key><string>NavCoreFFI</string>
  <key>CFBundleExecutable</key><string>NavCoreFFI</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>MinimumOSVersion</key><string>15.0</string>
</dict>
</plist>
PL
}

echo "==> frameworks"
FW_SIM=target/ios-frameworks/simulator
FW_DEV=target/ios-frameworks/device
make_framework "$SIM_OUT/$LIB" "$FW_SIM"
if [[ -z "${SKIP_DEVICE:-}" ]]; then
  make_framework "target/$DEVICE_TARGET/release/$LIB" "$FW_DEV"
fi

echo "==> xcframework"
XCF=ios/NavSDK/NavCoreFFI.xcframework
rm -rf "$XCF"
ARGS=(-framework "$FW_SIM/NavCoreFFI.framework")
if [[ -z "${SKIP_DEVICE:-}" ]]; then
  ARGS+=(-framework "$FW_DEV/NavCoreFFI.framework")
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
