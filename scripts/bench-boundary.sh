#!/usr/bin/env bash
# Phase 14: measures the FFI boundary cost of both approaches on the host JVM
# (UniFFI+JNA vs BoltFFI+JNI) and, on macOS with a simulator, in Swift.
# Prints the tables; the JVM run also writes android/demo/build/boundary-bench.{json,md}.
#
#   scripts/bench-boundary.sh                 # JVM + Swift (simulator UDID auto-picked)
#   SKIP_IOS=1 scripts/bench-boundary.sh      # JVM only
#   SIM_UDID=<udid> scripts/bench-boundary.sh # choose the simulator
#
# Prerequisites: cargo build -p navcore-ffi --release; scripts/build-boltffi-android.sh;
# for Swift: scripts/build-ios.sh and scripts/build-boltffi-ios.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> JVM"
(cd android && ./gradlew --console=plain -q :demo:testDebugUnitTest --tests '*BoundaryBenchmarkTest*' --rerun-tasks) \
  | sed -n '/BOUNDARY BENCH/,/^$/p'

if [[ -n "${SKIP_IOS:-}" ]] || [[ "$(uname -s)" != Darwin ]]; then
  exit 0
fi

echo "==> Swift (simulator)"
UDID="${SIM_UDID:-$(xcrun simctl list devices available -j | python3 -c '
import json,sys
d=json.load(sys.stdin)["devices"]
c=[(rt,x["name"],x["udid"]) for rt,v in d.items() if "iOS" in rt for x in v if x["isAvailable"] and x["name"].startswith("iPhone") and "Air" not in x["name"]]
print(sorted(c)[-1][2])')}"
OUT="$PWD/ios/Demo/boundary-bench.md"
(cd ios/Demo && TEST_RUNNER_NAVSDK_BENCH_OUT="$OUT" xcodebuild -scheme Demo \
  -destination "platform=iOS Simulator,id=$UDID" \
  -only-testing:DemoTests/BoundaryBenchmarkTests test 2>&1 | grep -E "BOUNDARY BENCH|^\| |TEST (SUCCEEDED|FAILED)") || true
[[ -f "$OUT" ]] && cat "$OUT"
