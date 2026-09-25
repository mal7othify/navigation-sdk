# Turn-by-Turn Navigation SDK POC — Implementation Plan

Rust core + UniFFI bindings → Kotlin (Android) and Swift (iOS).
This file is written for Claude Code. Work phase by phase, run the checks at the end of each phase, and do not start the
next phase until the current one passes.

## Goals

- Prove the pattern: one Rust core, consumed natively from Android and iOS.
- Accurate on-device location processing: Kalman filtering, HMM map matching against the route, hysteresis for step
  advancement and off-route detection.
- Coarse-grained, allocation-light FFI so the boundary is never the bottleneck.
- Pure core testable with `cargo test` and no mobile toolchain.

Out of scope for the POC: map rendering, a routing engine, TTS, background services. Rerouting is stubbed as a callback
to the host.

## Repo layout

```
navsdk/
  Cargo.toml                 # workspace
  rust-toolchain.toml        # pin stable
  crates/
    navcore/                 # pure logic, no FFI, no_std-friendly where practical
      src/{lib.rs, geo.rs, filter.rs, matcher.rs, navigator.rs, types.rs}
      tests/
      benches/
    navcore-ffi/             # UniFFI layer only; thin wrappers over navcore
      src/lib.rs
      uniffi.toml
  uniffi-bindgen/            # tiny bin crate: `cargo run -p uniffi-bindgen -- generate ...`
  android/
    navsdk/                  # Android library module (AAR)
    demo/                    # demo app
  ios/
    NavSDK/                  # Swift Package wrapping the XCFramework
    Demo/                    # demo app
  scripts/
    build-android.sh
    build-ios.sh
    gen-bindings.sh
  fixtures/
    route_simple.json        # short route + simulated GPS trace (with noise)
    route_parallel_roads.json
    route_uturn.json
  CLAUDE.md
  README.md
```

## Working in this repo

Build and test (no mobile toolchain needed for the core):

```
cargo build --workspace          # all crates
scripts/check.sh                 # fmt --check, clippy -D warnings, test  (must pass before every commit)
cargo test -p navcore            # core only
cargo bench -p navcore           # geometry / matcher / navigator benchmarks (Phase 1+)
scripts/gen-bindings.sh          # regenerate Kotlin + Swift bindings (Phase 5+)
scripts/build-android.sh         # cross-compile + bindings → android/navsdk (Phase 6+)
scripts/build-ios.sh             # XCFramework + bindings → ios/NavSDK (Phase 7+)
```

Layout notes:

- The repo root *is* the `navsdk/` root shown below; there is no nested folder.
- `crates/navcore` is the core: `#![deny(unsafe_code)]`, no FFI, `serde` only behind the `serde` feature.
- `crates/navcore-ffi` is the UniFFI layer: type conversion and forwarding only, no logic, no `unwrap`/`expect`.
- `uniffi-bindgen/` is the bindgen binary. Run it via `scripts/gen-bindings.sh`, not by hand.

**Never edit generated binding files.** `navcore.kt`, `navcore.swift`, `navcoreFFI.h` and the modulemap are
gitignored and produced by `scripts/gen-bindings.sh`. To change the API: edit Rust in `navcore-ffi`, rebuild,
regenerate, then update the Kotlin/Swift wrappers.

Decisions made while resolving gaps in the plan (keep consistent):

- `TripState` has a `needs_reroute: bool` field (set by the core, cleared on `set_route`).
- `TripProgress` is an enum: `NotStarted`, `Navigating`, `Arrived`.
- All tunables live in one `NavigatorConfig` struct with `Default` impl matching the values in this document.
- Geometry is hand-written (haversine, bearing, segment projection); no `geo` crate.
- Hot-loop scratch buffers (HMM window, candidate lists) are fixed-capacity and owned by `Navigator`.

Benchmark baselines (Apple Silicon, release, `cargo bench -p navcore`):

| Bench | Phase | Result |
|---|---|---|
| `route_index_build_2000` | 1 | ~346 µs |
| `nearest_segment_2000` | 1 | ~138 ns |
| `segments_within_50m_2000` | 1 | ~103 ns |
| `haversine` | 1 | ~18 ns |
| `hmm_match_fix_2000` | 3 | ~360 ns |
| `simple_match_fix_2000` | 3 | ~152 ns |

Fixture results (raw fixes, σ = reported accuracy): `route_parallel_roads` HMM 0 wrong-carriageway fixes vs simple 9;
`route_uturn` HMM 0 wrong-lane fixes vs simple 18. Kalman on `route_simple`: raw RMS 12.7 m → filtered 4.9 m (−61 %).

## Global conventions

- Rust stable, `edition = "2021"`, `#![deny(unsafe_code)]` in `navcore` (all unsafe stays in generated UniFFI code).
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test` must pass before every commit.
- Units: metres, seconds, degrees for lat/lng, radians internally for bearings. Document the unit in every field name or
  doc comment.
- No panics across the FFI boundary. Every fallible operation returns `Result<_, NavError>`; `NavError` is a UniFFI
  error enum.
- `navcore` has zero knowledge of UniFFI. `navcore-ffi` only converts types and forwards calls.
- Commit after each phase with a message `phase N: <summary>`.

---

## Phase 0 — Workspace and tooling

1. `cargo new --lib crates/navcore`, `cargo new --lib crates/navcore-ffi`, `cargo new uniffi-bindgen` (bin), wire the
   workspace `Cargo.toml`.
2. Add `rust-toolchain.toml` (stable channel) and `.gitignore` (target/, *.so, *.a, build dirs, generated bindings).
3. `CLAUDE.md` with: how to build/test, the conventions above, and "never edit generated binding files".
4. Install checks script `scripts/check.sh` running fmt, clippy, test.

**Done when:** `cargo build --workspace` and `scripts/check.sh` pass on an empty crate set.

---

## Phase 1 — Core types and geometry (`navcore`)

Dependencies: `geo` (or hand-written haversine to keep it lean), `rstar`, `serde` + `serde_json` (for fixtures/tests
only, behind a feature).

Types (`types.rs`):

```rust
pub struct GeoPoint {
    pub lat: f64,
    pub lng: f64
}
pub struct RawLocation {
    pub point: GeoPoint,
    pub accuracy_m: Option<f64>,
    pub speed_mps: Option<f64>,
    pub course_deg: Option<f64>,
    pub timestamp_ms: u64
}
pub enum ManeuverType { Depart, TurnLeft, TurnRight, SlightLeft, SlightRight, Straight, UTurn, Arrive }
pub struct RouteStep {
    pub instruction: String,
    pub maneuver: ManeuverType,
    pub start_index: usize,
    pub end_index: usize,
    pub distance_m: f64
}
pub struct Route {
    pub geometry: Vec<GeoPoint>,
    pub steps: Vec<RouteStep>
}
pub struct SnappedLocation {
    pub point: GeoPoint,
    pub segment_index: usize,
    pub distance_along_route_m: f64,
    pub distance_from_route_m: f64,
    pub bearing_deg: f64
}
pub struct TripState {
    pub snapped: SnappedLocation,
    pub current_step_index: usize,
    pub distance_to_next_maneuver_m: f64,
    pub distance_remaining_m: f64,
    pub next_instruction: String,
    pub is_off_route: bool,
    pub progress: TripProgress
}
```

Geometry (`geo.rs`):

- Haversine distance, initial bearing, project point onto segment (with along-track and cross-track distance),
  interpolate along polyline.
- `RouteIndex`: precomputed cumulative distance per vertex + `rstar::RTree` over segments (bounding boxes) for O (log n)
  candidate lookup.

Tests: known distances (e.g. two landmarks ~1 km apart), projection on/off segment, cumulative distance monotonic,
R-tree returns the correct segment for points near vertices and segment interiors.

**Done when:** tests pass; `cargo bench` has a baseline for "nearest segment on a 2,000-point route".

---

## Phase 2 — Location filter (`filter.rs`)

Implement a constant-velocity Kalman filter over a local ENU frame (convert lat/lng to metres around the route origin to
avoid trig in the hot loop).

- State: `[x, y, vx, vy]`. Measurement: position, optional speed+course used as a second measurement when present.
- Measurement noise from `accuracy_m` (fallback default 10 m); process noise tuned via a constant, exposed in
  `NavigatorConfig`.
- Reject fixes older than the last accepted timestamp and fixes implying impossible speed (> `config.max_speed_mps`).
- Output a `FilteredLocation` with position, velocity and heading.

Tests: noisy synthetic trace around a straight line converges within N updates; stale fix is rejected; teleport fix is
rejected.

**Done when:** RMS error on `fixtures/route_simple.json` (which includes ground truth) drops by ≥ 40% vs raw.

---

## Phase 3 — Map matcher (`matcher.rs`)

Implement HMM map matching (Newson–Krumm) restricted to the route polyline:

- Candidates per fix: segments returned by the R-tree within `config.candidate_radius_m` (default 50 m, widen once to
  150 m if none).
- Emission probability: Gaussian on cross-track distance with σ = max (accuracy, 5 m).
- Transition probability: exponential on |great-circle distance between fixes − along-route distance between
  candidates|, plus a heading-agreement term.
- Keep a bounded window (default 10 fixes) and run Viterbi over it; the result for the newest fix is the current match.
- Hard rule: prefer forward progress; candidates behind the last confirmed position by more than
  `config.max_backtrack_m` are penalised heavily (handles U-turn routes correctly because the route itself doubles
  back).

Also implement `SimpleMatcher` (nearest segment only) behind a trait so tests can compare the two.

Tests using fixtures:

- `route_parallel_roads.json`: two parallel segments 30 m apart; HMM picks the correct one using heading, simple matcher
  does not.
- `route_uturn.json`: matcher stays on the outbound leg until the U-turn is actually taken.
- Determinism: same input → identical output.

**Done when:** all fixture tests pass and matching one fix on a 2,000-point route is < 200 µs in the benchmark.

---

## Phase 4 — Navigator state machine (`navigator.rs`)

```rust
pub struct NavigatorConfig {
    step_advance_distance_m: 20.0,
    off_route_distance_m: 30.0,
    off_route_min_consecutive: 3,
    min_time_between_reroutes_s: 10.0,
    ...
}
pub struct Navigator {
    /* route, index, filter, matcher, state */
}
impl Navigator {
    pub fn new(route: Route, config: NavigatorConfig) -> Result<Self, NavError>;
    pub fn update_location(&mut self, raw: RawLocation) -> Result<TripState, NavError>;
    pub fn set_route(&mut self, route: Route) -> Result<(), NavError>;   // used after host reroutes
    pub fn state(&self) -> Option<&TripState>;
}
```

Rules:

- Step advances when snapped position passes the step's end vertex or is within `step_advance_distance_m` of it **and**
  heading roughly matches the next step (hysteresis, no flapping).
- Off-route becomes true only after `off_route_min_consecutive` fixes beyond `off_route_distance_m`; becomes false after
  2 consecutive good fixes.
- Reroute requests are rate-limited; the core sets `TripState.needs_reroute = true`, the host fetches a new route and
  calls `set_route`.
- Arrival when within `config.arrival_distance_m` of the last vertex on the last step.
- Hot loop: no `Vec` allocations per update; reuse scratch buffers held in `Navigator`.

Tests: full simulated drives over each fixture assert the sequence of `current_step_index`, off-route transitions, and
arrival. Property test (`proptest`): random noise never causes `distance_remaining_m` to increase by more than the noise
bound between consecutive on-route fixes.

**Done when:** a 10-minute simulated drive at 1 Hz runs in < 50 ms total in release mode.

---

## Phase 5 — UniFFI layer (`navcore-ffi`)

- Use UniFFI proc-macros (`#[derive(uniffi::Record)]`, `#[derive(uniffi::Enum)]`, `#[derive(uniffi::Object)]`,
  `#[derive(uniffi::Error)]`), not a `.udl` file.
- `uniffi::setup_scaffolding!()`; crate-type `["cdylib", "staticlib"]`.
- Expose exactly: `Navigator` object, `RawLocation`, `Route`/`RouteStep`/`GeoPoint` records, `TripState` record,
  `NavigatorConfig` record with defaults, `NavError`.
- `Navigator` is `Send + Sync` via an internal `Mutex`; host may call from any thread.
- Add `Route::from_json(&str)` helper so demo apps can load fixtures without building routes field by field.
- `uniffi-bindgen` crate: the standard 3-line `uniffi::uniffi_bindgen_main()` binary.
- `scripts/gen-bindings.sh`: generates Kotlin into `android/navsdk/src/main/kotlin/` and Swift into
  `ios/NavSDK/Sources/`, using the built library for metadata (`--library` mode).

**Done when:** `cargo build -p navcore-ffi --release` succeeds and `gen-bindings.sh` produces `navcore.kt` and
`navcore.swift` + `navcoreFFI.h` + modulemap.

---

## Phase 6 — Android

Tooling: `cargo-ndk`, NDK r26+, targets `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android`.

1. `scripts/build-android.sh`:
   `cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -o android/navsdk/src/main/jniLibs build --release -p navcore-ffi`,
   then run `gen-bindings.sh`.
2. `android/navsdk` library module: Kotlin, `net.java.dev.jna:jna:5.x@aar`, `kotlinx-coroutines-core`. Add a small
   idiomatic wrapper:
    - `NavigationSession(route, config)` exposing `StateFlow<TripState?>`.
    - `fun update(location: android.location.Location)` converts to `RawLocation` and calls Rust on
      `Dispatchers.Default` (never main thread).
    - `close()` calls `destroy()` on the UniFFI object.
3. Gradle: `ndk.abiFilters`, `packagingOptions` keep `libnavcore_ffi.so`, ProGuard/R8 keep rules for JNA and the
   generated package.
4. `android/demo`: single Compose screen that loads `fixtures/route_simple.json` from assets, replays the GPS trace at 1
   Hz (with a toggle for real `FusedLocationProvider`), and shows: next instruction, distance to manoeuvre, remaining
   distance, off-route badge.
5. Instrumented test: replay fixture, assert arrival.

**Done when:** `./gradlew :navsdk:assembleRelease :demo:assembleDebug` succeeds and the demo reaches "Arrive".

---

## Phase 7 — iOS

Targets: `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`. Min iOS 15.

1. `scripts/build-ios.sh`: build `staticlib` for each target, `lipo` the two simulator slices, run `gen-bindings.sh`,
   then `xcodebuild -create-xcframework` with the `.a` + headers + modulemap → `ios/NavSDK/NavCoreFFI.xcframework`.
2. `ios/NavSDK` Swift Package: binary target `NavCoreFFI` + Swift target `NavSDK` containing the generated
   `navcore.swift` and an idiomatic wrapper:
    - `@MainActor final class NavigationSession: ObservableObject` with `@Published var state: TripState?`.
    - `func update(_ location: CLLocation)` converts and calls Rust on a background `DispatchQueue`/`Task.detached`,
      publishes on main.
    - Map `NavError` to a Swift `Error`.
3. `ios/Demo`: SwiftUI app mirroring the Android demo (fixture replay + `CLLocationManager` toggle).
4. XCTest: replay fixture, assert arrival.

**Done when:** `xcodebuild -scheme Demo -destination 'platform=iOS Simulator,name=iPhone 15' build test` passes.

---

## Phase 8 — CI and docs

- GitHub Actions: `check` (fmt/clippy/test/bench-compile) on ubuntu; `android` job with NDK building the AAR; `ios` job
  on macos building the XCFramework and running the simulator test. Cache cargo and Gradle.
- `README.md`: architecture diagram (text), how to build each platform, how to add a new API method (edit Rust → regen
  bindings → wrappers), tuning guide for `NavigatorConfig`.
- Record baseline numbers from Phase 3/4 benchmarks in the README.

---

## Acceptance checklist

- [ ] `scripts/check.sh` green
- [ ] HMM matcher beats simple matcher on `route_parallel_roads` and `route_uturn`
- [ ] Kalman reduces RMS error ≥ 40% on `route_simple`
- [ ] No allocations in `update_location` hot path (verify with `dhat` or a counting allocator in a test)
- [ ] Both demos replay the fixture to arrival
- [ ] No panics reachable from FFI (grep for `unwrap`/`expect` in `navcore-ffi`; none allowed)
- [ ] Generated bindings are gitignored and reproducible from `gen-bindings.sh`

## Future (not in POC)

- Bundle Valhalla/OSRM for offline rerouting inside the core.
- IMU fusion (gyro/accelerometer) for tunnels.
- Wasm target for a web demo.
- Compare against Ferrostar on the same fixtures before deciding build-vs-adopt.