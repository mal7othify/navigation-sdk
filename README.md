# NavSDK — turn-by-turn navigation core (Rust → Kotlin / Swift)

Proof of concept for one Rust navigation core consumed natively on Android and iOS through
[UniFFI](https://mozilla.github.io/uniffi-rs/). The core does the hard on-device work — Kalman
filtering of GNSS fixes, HMM map matching against the route, hysteretic step advancement,
off-route detection and rate-limited reroute requests — and the mobile layers are thin, idiomatic
wrappers.

Out of scope for the POC: map rendering, routing, TTS, background services. Rerouting is a
callback to the host (`TripState.needs_reroute` → host fetches a route → `set_route`).

## Architecture

```
                 ┌──────────────────────────── navcore (pure Rust, no FFI) ────────────────────────────┐
 RawLocation ──▶ │ LocalFrame ──▶ LocationFilter ──▶ HmmMatcher ──▶ Navigator (steps / off-route / ...) │ ──▶ TripState
 (lat, lng,      │  (lat/lng →     (const-velocity   (windowed        RouteIndex: R-tree over          │     (snapped pos,
  accuracy,      │   local ENU      Kalman, 4-state)   Viterbi over     segments + cumulative           │      step, distances,
  speed, course) │   metres)                            route segs)     distance per vertex             │      flags)
                 └────────────────────────────────────────────────────────────────────────────────────┘
                                                        │
                                          navcore-ffi (UniFFI proc-macros, thin type conversion)
                                                        │
                        ┌───────────────────────────────┴───────────────────────────────┐
                        ▼                                                               ▼
        android/navsdk  (AAR)                                             ios/NavSDK  (Swift package)
        com.navsdk.NavigationSession → StateFlow<TripState?>              NavigationSession: ObservableObject
        Rust runs on Dispatchers.Default                                  Rust runs on a private serial queue
                        │                                                               │
                android/demo (Compose)                                          ios/Demo (SwiftUI)
```

Crates:

| Crate | Role |
|---|---|
| `crates/navcore` | Pure logic. `#![deny(unsafe_code)]`, no FFI, `serde` only behind a feature. Tested with `cargo test`. |
| `crates/navcore-ffi` | UniFFI records/enums/object + `From` conversions. No logic, no `unwrap`/`expect`. |
| `uniffi-bindgen` | `uniffi::uniffi_bindgen_main()` binary used by `scripts/gen-bindings.sh`. |

## Build & test

Core (no mobile toolchain needed):

```sh
scripts/check.sh                  # fmt --check, clippy -D warnings, all tests, FFI panic grep
cargo bench -p navcore            # geo / matcher / navigator benchmarks
cargo run -p navcore --features serde --example gen_fixtures   # regenerate fixtures/*.json
```

Android (needs NDK, `cargo install cargo-ndk`, rustup targets `aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`, JDK 17+):

```sh
scripts/build-android.sh          # .so for 3 ABIs → android/navsdk/src/main/jniLibs + Kotlin bindings
cd android
./gradlew :navsdk:assembleRelease :demo:assembleDebug   # AAR + demo APK
./gradlew :navsdk:testDebugUnitTest                     # runs the real core on the JVM via JNA
./gradlew :demo:connectedDebugAndroidTest               # replays fixtures on an emulator/device
```

iOS (needs Xcode 15+, rustup targets `aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`):

```sh
scripts/build-ios.sh              # static libs → lipo → ios/NavSDK/NavCoreFFI.xcframework + Swift bindings
UDID=$(xcrun simctl list devices available -j | python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; print(next(x["udid"] for rt,v in d.items() if "iOS" in rt for x in v if x["name"].startswith("iPhone")))')
xcodebuild -project ios/Demo/Demo.xcodeproj -scheme Demo -destination "platform=iOS Simulator,id=$UDID" build test
```

Both demos load `fixtures/*.json`, replay the recorded GPS trace at 1× or 10×, and show the next
instruction, distance to manoeuvre, remaining distance, and OFF ROUTE / ARRIVED / reroute badges.
A toggle switches to the device's real location provider (FusedLocationProvider / CLLocationManager).

Generated bindings (`navcore.kt`, `NavCore.swift`, `NavCoreFFI.h`, modulemap) are gitignored and
reproduced by `scripts/gen-bindings.sh`. **Never edit them.**

## Adding an API method

1. Implement it in `crates/navcore` with tests.
2. Expose it in `crates/navcore-ffi/src/lib.rs`: add the method to the `#[uniffi::export] impl Navigator`
   block (or a free `#[uniffi::export] fn`), converting types with `From` impls. Return
   `Result<_, NavError>` for anything fallible; never `unwrap`.
3. `cargo build -p navcore-ffi --release && scripts/gen-bindings.sh` — the Kotlin and Swift APIs now have it.
4. Surface it in the wrappers: `android/navsdk/.../NavigationSession.kt` and
   `ios/NavSDK/Sources/NavSDK/NavigationSession.swift` (dispatch off the main thread, publish results).
5. `scripts/check.sh`, then the platform builds above.

## Tuning `NavigatorConfig`

All tunables live in one record with defaults; hosts override selectively
(`NavigatorConfig(offRouteDistanceM = 40.0)` in Kotlin, `NavigatorConfig(offRouteDistanceM: 40)` in Swift).

| Field | Default | Effect |
|---|---|---|
| `step_advance_distance_m` | 20 | Advance early when this close to the step end **and** heading matches the next step. Larger = earlier instructions, more risk at short steps. |
| `step_advance_heading_tolerance_deg` | 45 | Heading tolerance for the early advance. |
| `off_route_distance_m` | 30 | Snapped distance from the route that counts as off-route. Raise in urban canyons. |
| `off_route_min_consecutive` / `on_route_min_consecutive` | 3 / 2 | Hysteresis: fixes needed to enter / leave off-route. |
| `min_time_between_reroutes_s` | 10 | Rate limit on `needs_reroute`. |
| `arrival_distance_m` | 15 | Arrival radius around the final vertex. |
| `max_speed_mps` | 70 | Fixes implying faster movement are rejected as teleports (after 5 in a row the filter re-initialises). |
| `process_noise_accel_mps2` | 1.0 | Kalman process noise. Lower = smoother, laggier through turns. 0.5 for highways, 2 for city driving. |
| `default_accuracy_m` | 10 | Assumed σ for fixes without accuracy. |
| `candidate_radius_m` / `widened_candidate_radius_m` | 50 / 150 | Map-matching search radii. |
| `max_backtrack_m` | 30 | Backwards movement along the route tolerated before penalties. Keep ≥ 2× typical accuracy. |
| `hmm_window_size` | 10 | Viterbi window in fixes (≤ 16). Longer = more inertia, slower recovery from a wrong branch. |

## Fixtures

`fixtures/*.json` hold a route, a simulated 1 Hz GPS trace with noise, per-fix ground truth and drive-level
expectations. They are generated deterministically by `crates/navcore/examples/gen_fixtures.rs`:

| Fixture | Exercises |
|---|---|
| `route_simple` | 3-step urban route; Kalman RMS reduction; arrival. |
| `route_parallel_roads` | Divided road, return carriageway 30 m away: heading + continuity pick the right side. |
| `route_uturn` | Return lane 8 m away, 6 s wait at the apex: matcher holds the outbound leg until the turn is taken. |
| `route_detour` | Overshoots a turn by 150 m: off-route hysteresis, rate-limited reroute requests, recovery. |

## Baseline numbers (Apple M-series, release)

| Measurement | Result | Target |
|---|---|---|
| Kalman on `route_simple`, RMS position error | 12.7 m raw → 4.9 m filtered (−61 %) | ≥ 40 % reduction |
| `route_parallel_roads`, wrong-carriageway fixes | HMM 0 vs nearest-segment 9 | HMM beats simple |
| `route_uturn`, wrong-lane fixes | HMM 0 vs nearest-segment 18 | HMM beats simple |
| `nearest_segment_2000` (R-tree lookup, 2,000-vertex route) | ~138 ns | — |
| `hmm_match_fix_2000` (one fix, 2,000-vertex route) | ~360 ns | < 200 µs |
| `navigator_update_location` | ~0.8 µs | — |
| `navigator_drive_600_fixes` (10 min @ 1 Hz) | ~0.4 ms | < 50 ms |
| Allocations per `update_location` after warm-up | 0 (counting allocator test) | 0 |

## Layout

```
crates/navcore/           core: geo.rs filter.rs matcher.rs navigator.rs types.rs fixtures.rs
crates/navcore-ffi/       UniFFI layer + uniffi.toml
uniffi-bindgen/           bindgen binary
android/navsdk, demo/     AAR module + Compose demo
ios/NavSDK, Demo/         Swift package (+ XCFramework) + SwiftUI demo (XcodeGen project.yml)
fixtures/                 generated test routes + traces
scripts/                  check.sh gen-bindings.sh build-android.sh build-ios.sh
```

## Future (not in the POC)

Offline rerouting (Valhalla/OSRM in the core), IMU fusion for tunnels, a wasm target for a web demo,
and a side-by-side comparison with Ferrostar on the same fixtures before deciding build vs adopt.
