# NavSDK — turn-by-turn navigation core (Rust → Kotlin / Swift)

Proof of concept for one Rust navigation core consumed natively on Android and iOS, built **two
ways on the same core** so the binding toolkit and the delivery model can be compared with numbers
(see [Pull vs Push](#pull-vs-push)):

- **A, pull** — [UniFFI](https://mozilla.github.io/uniffi-rs/) over JNA: the host calls
  `update_location` and gets the whole state back; the host acts on reroute requests.
- **B, push** — [BoltFFI](https://github.com/boltffi/boltffi) over plain JNI: the host feeds
  locations and subscribes to listeners; Rust calls back with a position every fix, a trip state
  only when it changes, and owns rerouting by calling the host's `ReplanningService`.

The core does the hard on-device work — Kalman filtering of GNSS fixes, HMM map matching against
the route, hysteretic step advancement, off-route detection with grace period and movement gates,
and a bounded, rate-limited reroute policy — and the mobile layers are thin, idiomatic wrappers.

Out of scope for the POC: map rendering, routing, TTS, background services. Route fetching is
always the host's job; the two approaches differ in who *decides* and who *installs* the route.

## Architecture

```
                 ┌──────────────────────────── navcore (pure Rust, no FFI) ────────────────────────────┐
 RawLocation ──▶ │ LocalFrame ──▶ LocationFilter ──▶ HmmMatcher ──▶ Navigator (steps / off-route / ...) │ ──▶ TripState
 (lat, lng,      │  (lat/lng →     (const-velocity   (windowed        RouteIndex: R-tree over          │     (snapped pos,
  accuracy,      │   local ENU      Kalman, 4-state)   Viterbi over     segments + cumulative           │      step, distances,
  speed, course) │   metres)                            route segs)     distance per vertex             │      flags)
                 └────────────────────────────────────────────────────────────────────────────────────┘
                                   Update { PositionState (every fix), TripState, trip_changed }
                          ┌─────────────────────────────┴──────────────────────────────┐
                          ▼                                                            ▼
      A · navcore-ffi (UniFFI + JNA, pull)                         B · navcore-boltffi (BoltFFI + JNI, push)
      Navigator.update_location → FullState                        NavigationController.update_location → ()
      host sees TripState.reroute, calls set_route                 Rust calls NavigationListener.on_position/on_trip/on_session
                                                                   Rust calls ReplanningService.replan (async), installs the route
          ┌───────────────┴───────────────┐                            ┌───────────────┴────────────────┐
          ▼                               ▼                            ▼                                ▼
  android/navsdk (AAR)          ios/NavSDK (SwiftPM)         android/navsdk-push (AAR)         ios/NavSDKPush (SwiftPM)
  NavigationSession             NavigationSession            NavigationSessionPush             NavigationSessionPush
  position/trip StateFlows      @Published position/trip     position/trip/session StateFlows  @Published position/trip/session
          └───────────────┬───────────────┘                            └───────────────┬────────────────┘
                 android/demo (Compose) · ios/Demo (SwiftUI): tabs Pull / Push / Both (lockstep + divergence flag)
```

Crates:

| Crate | Role |
|---|---|
| `crates/navcore` | Pure logic. `#![deny(unsafe_code)]`, no FFI, `serde` only behind a feature. Tested with `cargo test`. Includes `sim.rs`, a seeded route simulator shared by tests, demos and the fixture generator. |
| `crates/navcore-ffi` | **A, pull.** UniFFI records/enums/objects + `From` conversions. No logic, no `unwrap`/`expect`. |
| `crates/navcore-boltffi` | **B, push.** BoltFFI records/classes/callback traits; a `NavigationController` that fans out to listeners and awaits the host's replanner on a helper thread. No logic, no `unwrap`/`expect`. |
| `uniffi-bindgen` | `uniffi::uniffi_bindgen_main()` binary used by `scripts/gen-bindings.sh`. |

## Build & test

Core (no mobile toolchain needed):

```sh
scripts/check.sh                  # fmt --check, clippy -D warnings, all tests, FFI panic grep
cargo bench -p navcore            # geo / matcher / navigator benchmarks
cargo run -p navcore --features serde --example gen_fixtures   # regenerate fixtures/*.json
```

Android (needs NDK, `cargo install cargo-ndk boltffi_cli`, rustup targets `aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`, JDK 17+):

```sh
scripts/build-android.sh          # A: .so for 3 ABIs → android/navsdk/src/main/jniLibs + UniFFI Kotlin bindings
scripts/build-boltffi-android.sh  # B: boltffi pack android → android/navsdk-push (jniLibs, generated/, desktop JNI lib)
cd android
./gradlew :navsdk:assembleRelease :navsdk-push:assembleRelease :demo:assembleDebug   # both AARs + demo APK
./gradlew :navsdk:testDebugUnitTest :navsdk-push:testDebugUnitTest                   # real core on the JVM (JNA / JNI)
./gradlew :demo:connectedDebugAndroidTest               # replays fixtures through both on an emulator/device
```

iOS (needs Xcode 15+, `cargo install boltffi_cli`, rustup targets `aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`):

```sh
scripts/build-ios.sh              # A: static frameworks → ios/NavSDK/NavCoreFFI.xcframework + Swift bindings
scripts/build-boltffi-ios.sh      # B: boltffi pack apple → ios/NavSDKPush/NavCorePush.xcframework + Swift bindings
UDID=$(xcrun simctl list devices available -j | python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; print(next(x["udid"] for rt,v in d.items() if "iOS" in rt for x in v if x["name"].startswith("iPhone")))')
xcodebuild -project ios/Demo/Demo.xcodeproj -scheme Demo -destination "platform=iOS Simulator,id=$UDID" build test
```

Both demos have three tabs. **Pull** and **Push** each drive their own session; **Both** feeds one
trace to both sessions in lockstep, shows two cards, and flags the moment their snapped positions
differ by more than 0.5 m (they never do). Sources: the recorded fixture trace at 1× or 10×, the
Rust simulator (with a *Deviate* button that leaves the route for 300 m to trigger a reroute), or
the device's real location provider. Each card carries a metrics strip: boundary µs per call
(avg / p99 / max), position and trip updates delivered, instruction-card recompositions, reroutes
and routes installed.

Generated bindings are gitignored and reproduced by the build scripts: UniFFI's `navcore.kt`,
`NavCore.swift`, `NavCoreFFI.h` + modulemap by `scripts/gen-bindings.sh`; BoltFFI's
`android/navsdk-push/generated/` and `ios/NavSDKPush/Sources/BoltFFI/` by `boltffi pack`.
**Never edit them.**

### Sharing the demo APK

CI does not publish the APK on every push. To get a downloadable build, run the manual
`Demo APK` workflow (Actions tab → *Demo APK* → *Run workflow*, or from a terminal):

```sh
gh workflow run demo-apk.yml            # optionally: -f retention_days=7
gh run list --workflow=demo-apk.yml     # find the run
gh run download <run-id>                # fetches navsdk-demo-debug-<sha>.apk
```

The APK is a debug build signed with the CI debug keystore and bundles all three ABIs. Testers
install it via `adb install -r` or by opening the file on the phone and allowing installs from
that source.

## Adding an API method

1. Implement it in `crates/navcore` with tests.
2. Expose it in both binding crates, converting types with `From` impls and returning
   `Result<_, NavError>` for anything fallible (never `unwrap`):
   - A: `crates/navcore-ffi/src/lib.rs`, in the `#[uniffi::export] impl Navigator` block or as a free
     `#[uniffi::export] fn`.
   - B: `crates/navcore-boltffi/src/lib.rs`, in the `#[export] impl NavigationController` block or as a
     free `#[export] fn`; if the host should be *told* something, add a method to `NavigationListener`.
3. Regenerate: `cargo build -p navcore-ffi --release && scripts/gen-bindings.sh` for A;
   `scripts/build-boltffi-android.sh` / `scripts/build-boltffi-ios.sh` for B.
4. Surface it in the wrappers (`NavigationSession` in `android/navsdk` and `ios/NavSDK`;
   `NavigationSessionPush` in `android/navsdk-push` and `ios/NavSDKPush`), dispatching off the main thread.
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
| `off_route_min_speed_mps` | 1.0 | Off-route fixes only count while moving at least this fast: a parked phone drifting under a bridge never goes off-route. |
| `off_route_min_movement_m` | 5 | The vehicle must have moved this far since the streak began before off-route flips. |
| `reroute_grace_period_s` | 10 | Deviations are ignored this long after `new` / `set_route` (filter settling, leaving a car park). |
| `suppress_off_route_near_destination_m` | 50 | No deviations this close to the destination, where the last metres are rarely on the polyline. |
| `min_time_between_reroutes_s` | 10 | Gap between reroute requests; also the retry interval while the host has not installed a route. |
| `max_reroute_attempts` | 3 | Unanswered requests before the core reports one `RerouteReason::GaveUp` and stops asking. `0` = no limit. `set_route` resets it; rejoining the route on your own clears it. |
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

## Pull vs Push

Same core, same fixtures, same metrics; only the binding toolkit and the delivery model differ.
Measured on an Apple M-series laptop on 2026-10-11 with `scripts/bench-boundary.sh` and
`scripts/measure-sizes.sh` (release builds, 10 000 `update_location` calls per row after a
200-call warm-up; the core itself costs ~0.8 µs per call).

### FFI boundary cost, µs per `update_location`

Pull returns the whole `FullState` record; push returns nothing but runs the `on_position`
callback (and `on_trip` when it changed) before returning, so both rows carry one state record
across the boundary per call.

| Platform | Case | A: pull (UniFFI) avg / p50 / p99 | B: push (BoltFFI) avg / p50 / p99 |
|---|---|---|---|
| JVM (JNA vs JNI) | `route_simple`, 47 vertices | 18.0 / 11.4 / 66.4 | 0.8 / 0.8 / 1.1 |
| JVM (JNA vs JNI) | straight route, 2 000 vertices | 17.4 / 11.8 / 25.5 | 1.2 / 1.3 / 1.7 |
| Swift (simulator) | `route_simple`, 47 vertices | 5.3 / 5.2 / 6.8 | 1.2 / 1.1 / 1.6 |
| Swift (simulator) | straight route, 2 000 vertices | 5.7 / 5.7 / 7.5 | 1.6 / 1.6 / 2.2 |

Reading it: on the JVM, JNA costs about 10 µs per call on top of the ~1 µs that JNI plus the
wire encoding costs; pull also showed rare 20–40 ms outliers (JIT / GC pauses in the JNA path)
that push did not. In Swift the gap is ~4 µs. At 1 Hz GPS neither matters; at IMU rates
(100–200 Hz, or batches) only push stays below 1 % of a core budget on the JVM. Route size
barely moves either number because the route crosses once at construction, not per fix.

### Size and code volume

| Metric | A: pull (UniFFI) | B: push (BoltFFI) |
|---|---|---|
| Android `.so` arm64-v8a, as built | 888 KB (stripped by cargo-ndk) | 5 892 KB (unstripped, with debug info) |
| Android `.so` arm64-v8a after `llvm-strip --strip-all` (what ships) | 612 KB | 572 KB |
| Extra Android runtime deps | JNA AAR (~1.5 MB) + androidx.annotation | none (plain JNI) |
| iOS XCFramework on disk, all slices | 42 MB | 73 MB (unstripped) |
| Generated Kotlin, lines | 3 602 | 2 305 + 1 350 lines of C JNI glue |
| Generated Swift, lines | 2 820 | 1 473 |
| Hand-written FFI crate, lines | 774 | 1 077 (listeners, replan thread, session state) |
| Kotlin / Swift wrapper, lines | 169 / 144 | 193 / 236 |

### What the demo shows on a `route_simple` replay (216 fixes, 4 steps)

| | A: pull | B: push |
|---|---|---|
| Position updates delivered | 216 | 216 |
| Trip updates delivered | 4–5 | 4–5 (identical: both come from the core's `trip_changed`) |
| Instruction card recompositions | 4–5 | 4–5 |
| Snapped positions | identical fix by fix (Both tab never flags divergence) | |
| Reroute on `route_detour` | host wrapper fetches and calls `setRoute` | core calls `replan`, installs the route, reports `routesInstalled` |

### Trade-offs that the numbers do not show

| | A: pull (UniFFI) | B: push (BoltFFI) |
|---|---|---|
| Simplicity | Every call is a plain function; trivially unit-testable; host owns all threading | Listener registry, callback threading rules (callbacks run on the feeding thread), a helper thread for replans |
| Rust-initiated events | Impossible: nothing happens without a host call | Natural: timers, replan completion, future IMU-driven updates |
| Policy ownership | Rerouting policy is in the core but the host must act on it; Kotlin and Swift can drift | Policy and action in Rust; the host only fetches routes |
| Toolkit maturity | Years in production (Firefox, Glean) | First release 2026-02; 0.31 as of 2026-09; API still moving |
| Toolkit gotchas met here | Two bare-library XCFrameworks in one app clash on `module.modulemap` (fixed by packaging static frameworks) | `#[error]` clashes with thiserror's helper; C glue lands next to the Kotlin; ships unstripped `.so`; generated Swift shares the wrapper's module |
| Async host callbacks | Supported | Supported (`#[async_trait]`); Rust needs a `block_on` (20 lines, no runtime) to await them off the host thread |

**Recommendation.** For a GPS-only navigator at 1 Hz, pull with UniFFI is the lower-risk choice:
less code, a mature toolkit, and the boundary cost is irrelevant. Push earns its complexity when
the core must own policy (rerouting, better-route polling, route refresh) or take high-rate sensor
input; at that point BoltFFI's JNI path is 10× cheaper on the JVM and removes the JNA dependency,
at the price of a young toolkit. The architecture choice (pull vs push) matters more than the
toolkit choice: the core's split state (`PositionState` every fix, `TripState` on change) is what
keeps UI work proportional to what changed, and both approaches get that for free.

## Layout

```
crates/navcore/              core: geo.rs filter.rs matcher.rs navigator.rs sim.rs types.rs fixtures.rs
crates/navcore-ffi/          A (pull): UniFFI layer + uniffi.toml
crates/navcore-boltffi/      B (push): BoltFFI layer + boltffi.toml (run the CLI from this directory)
uniffi-bindgen/              bindgen binary for A
android/navsdk/              A: AAR, NavigationSession (UniFFI + JNA)
android/navsdk-push/         B: AAR, NavigationSessionPush (BoltFFI + JNI); generated/ and jniLibs/ are gitignored
android/demo/                Compose demo with Pull / Push / Both tabs; src/test has the JVM boundary benchmark
ios/NavSDK/                  A: Swift package (+ XCFramework of static frameworks)
ios/NavSDKPush/              B: Swift package; Sources/BoltFFI and the XCFramework are gitignored
ios/Demo/                    SwiftUI demo (XcodeGen project.yml) with the same tabs; DemoTests has the Swift benchmark
docs/PLAN-v2-pull-vs-push.md the comparison plan and per-phase outcomes
fixtures/                    generated test routes + traces
scripts/                     check.sh gen-bindings.sh build-*.sh build-boltffi-*.sh bench-boundary.sh measure-sizes.sh
```

## Future (not in the POC)

Offline rerouting (Valhalla/OSRM in the core), IMU fusion for tunnels, a wasm target for a web demo,
and a side-by-side comparison with Ferrostar on the same fixtures before deciding build vs adopt.
