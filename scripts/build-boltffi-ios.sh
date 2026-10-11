#!/usr/bin/env bash
# Approach B (push): builds navcore-boltffi for iOS device + simulator,
# regenerates the Swift bindings and writes ios/NavSDKPush/NavCorePush.xcframework
# (all gitignored). Package.swift is hand-written and kept.
#
#   scripts/build-boltffi-ios.sh
#
# Requires: `cargo install boltffi_cli` and the Apple rustup targets
# (`boltffi check --apple` lists what is missing).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> boltffi pack apple --release"
(cd crates/navcore-boltffi && boltffi pack apple --release "$@")

# `generate swift` (without pack) drops a loose copy next to Sources/; only the
# bundled copy under Sources/BoltFFI belongs in the module.
rm -f ios/NavSDKPush/Sources/NavcoreBoltffiBoltFFI.swift ios/NavSDKPush/Sources/boltffi.h
rm -f ios/NavSDKPush/NavCorePush.xcframework.zip

echo "==> test fixtures"
FIX=ios/NavSDKPush/Tests/NavSDKPushTests/Fixtures
rm -rf "$FIX" && mkdir -p "$FIX"
cp fixtures/*.json "$FIX/"
DEMO_FIX=ios/Demo/Demo/Fixtures
mkdir -p "$DEMO_FIX"
cp fixtures/*.json "$DEMO_FIX/"

echo "==> done:"
ls ios/NavSDKPush/NavCorePush.xcframework
