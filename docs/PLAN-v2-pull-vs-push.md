# Plan v2 — Two implementations of the same Rust navigation core: Pull vs Push

One Rust core (`navcore`), two complete ways to ship it to Android and iOS, and a single demo
app that runs both side by side on the same GPS trace.

|                   | Approach A — **Pull**                                           | Approach B — **Push**                                                              |
|-------------------|-----------------------------------------------------------------|------------------------------------------------------------------------------------|
| Binding toolkit   | UniFFI 0.32 (proc-macros), JNA on Android                       | BoltFFI (JNI codegen, github.com/boltffi/boltffi, MIT)                             |
| Call model        | Host calls `update_location`, gets the full state back          | Host feeds locations, subscribes to listeners, Rust calls back                     |
| State granularity | One `TripState` per fix                                         | Three streams: position (every fix), trip (on change), session (rarely)            |
| Rerouting         | Host sees a `RerouteRequest` in the state and calls `set_route` | Core calls the host's `ReplanningService.replan()` and installs the result itself  |
| Simulation        | Host replays a fixture trace                                    | Rust `RouteSimulator` drives the session                                           |
| Threading         | Host owns the thread, calls are synchronous                     | Rust calls back on the feeding thread; async replans complete through Rust futures |

Approach A is what Phases 0–8 built. Approach B is new. Both consume the same `navcore`, the same
fixtures and the same metrics so the comparison isolates the binding layer and the delivery model.

This document has three parts: the comparison dimensions, the repo changes, and the phases.
Phases continue the numbering in `CLAUDE.md`.

**Status (2026-10-11): Phases 9–14 implemented.** Measured results live in `README.md` under
"Pull vs Push"; the per-phase notes below record what deviated from the plan.

---

## 1. What is being compared

### 1.1 Binding layer

| Dimension                  | A: UniFFI + JNA                                                                | B: BoltFFI (JNI)                                                                                                                                                     |
|----------------------------|--------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Marshalling                | Every record is lowered into a `RustBuffer` (heap alloc + copy) on both sides. | Records are written into a pooled, thread-local direct `ByteBuffer` and passed as pointer + length; results return as one `byte[]`; scalars cross as JNI primitives. |
| Objects                    | `Arc<T>` handle, `destroy()`                                                   | `long` handle wrapped in `AutoCloseable` with a closed flag                                                                                                          |
| Host-implemented callbacks | `#[uniffi::export(with_foreign)]` traits                                       | Generated `@JvmStatic` callback entry points plus a handle registry                                                                                                  |
| Async                      | Foreign-driven polling of Rust futures                                         | Same shape: poll / complete / cancel / free per future, resumes a coroutine                                                                                          |
| Panics                     | Caught by scaffolding, surfaced as an exception                                | Caught, surfaced as an exception with a message                                                                                                                      |
| Android deps               | JNA AAR (~1.5 MB) + `androidx.annotation`                                      | Kotlin stdlib + coroutines only                                                                                                                                      |
| Other targets              | Kotlin, Swift, Python, Ruby                                                    | Kotlin, Swift, Java, C#, Python, WASM/TypeScript                                                                                                                     |
| JVM unit tests             | Yes, JNA loads the host dylib                                                  | Yes, a desktop JNI library is loaded from resources                                                                                                                  |
| Maturity                   | Years in production (Firefox, Glean)                                           | First release 2026-02, 0.31.0 as of 2026-09; frequent releases                                                                                                       |

**Expected cost picture.** The core does one update in ~0.8 µs. A UniFFI+JNA round trip is
typically tens of µs; BoltFFI claims sub-µs. At 1 Hz GPS both are noise. The boundary matters
when IMU samples arrive at 100–200 Hz or when large records (routes, full step lists) cross per
update. Phase 13 measures this instead of trusting either claim.

### 1.2 Delivery model

| Dimension                | A: Pull                                                                               | B: Push                                                                               |
|--------------------------|---------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------|
| Simplicity               | Every call is a plain function; trivial to unit test                                  | Listener registry, handle tables, callback threading rules                            |
| Backpressure             | Natural: the host decides when to call                                                | The host must not block inside callbacks                                              |
| Re-serialisation per fix | Whole `TripState` (small today: ~80 bytes + one `Arc<str>`)                           | Position only; trip payload only when the step, instruction or off-route flag changes |
| Rust-initiated events    | Impossible: nothing happens without a host call                                       | Timers, replan completion and better-route polling can emit without a fix             |
| Policy ownership         | Rerouting policy in the core, but the host must act on it; Kotlin and Swift can drift | All policy and the action in Rust; the host only fetches routes                       |
| UI cost                  | Every fix recomposes everything bound to the state                                    | Instruction card recomposes only on trip changes                                      |

### 1.3 Shared, approach-neutral guarantees

Both approaches must keep: all config defaults in `NavigatorConfig::default()`, zero allocations in
the core hot loop (`tests/no_alloc.rs`), the fixture-proven matcher, `#![deny(unsafe_code)]` in
`navcore`, no `unwrap`/`expect` in either FFI crate.

---

## 2. Repo changes

```
crates/
  navcore/                 shared core: granular state, RerouteRequest policy, sim.rs
  navcore-ffi/             Approach A (UniFFI)  — existing, pull API
  navcore-boltffi/         Approach B (BoltFFI) — new, push API
uniffi-bindgen/            unchanged
android/
  navsdk/                  A wrapper: NavigationSession (pull)
  navsdk-push/             B wrapper: NavigationSessionPush (listeners, replanner lambda)
  demo/                    one app, "Pull" and "Push" tabs + "Both" mode with live metrics
ios/
  NavSDK/                  A
  NavSDKPush/              B (BoltFFI XCFramework + wrapper)
  Demo/                    segmented control: Pull / Push / Both
scripts/
  build-boltffi-android.sh, build-boltffi-ios.sh   (wrap `boltffi pack`)
  bench-boundary.sh        runs the JVM and XCTest boundary microbenchmarks
```

Generated BoltFFI Kotlin/Swift files are gitignored like the UniFFI ones. Never edit them.

---

## 3. Phases

Conventions from `CLAUDE.md` apply: `scripts/check.sh` green before each commit, defaults only in
Rust, never edit generated bindings. **Do not commit or push until asked.**

### Phase 9 — Shared core additions (`crates/navcore`)

Everything both approaches need, with no FFI knowledge.

1. **Granular state** in `types.rs`:
    - `PositionState { snapped, distance_to_next_maneuver_m, distance_remaining_m, is_off_route, progress }`
    - `TripState { current_step_index, next_instruction: Arc<str>, step_count, reroute: Option<RerouteRequest> }`
    - `FullState { position, trip }` as the pull-side return type and the single test assertion target.
    - `Navigator::update_location` returns `Update { position: PositionState, trip_changed: bool }`;
      `Navigator::trip()` and `Navigator::full_state()` accessors. No new allocations.
2. **Reroute policy** in `navigator.rs`:
    - `NavigatorConfig` gains `reroute_grace_period_s: 10.0`, `off_route_min_movement_m: 5.0`,
      `off_route_min_speed_mps: 1.0`, `max_reroute_attempts: 3`, `suppress_off_route_near_destination_m: 50.0`.
    - `RerouteRequest { origin, heading_deg, destination, attempt, reason }` replaces `needs_reroute`.
    - `reroute_in_flight` guard: one pending request at a time; attempts reset on `set_route`;
      `RerouteReason::GaveUp` after the limit.
3. **Simulator** in `sim.rs` (no `rand`; xorshift64\* + Box–Muller, seeded, deterministic):
    -
    `SimulatorConfig { speed_mps: 13.9, fix_interval_s: 1.0, position_sigma_m: 8.0, reported_accuracy_m: 10.0, speed_sigma_mps: 0.5, course_sigma_deg: 5.0, seed }`
    - `RouteSimulator::new(route, config)`, `advance(dt_s) -> RawLocation`, `set_target_speed`,
      `set_planned_deviation(at_m, lateral_m, length_m)`, `start/stop_signal_degradation`, `is_finished`.
    - `gen_fixtures` switches to it; fixtures are regenerated once and reviewed.
4. Tests: trip-change count on `route_simple` equals step advances + off-route transitions; at
   most one pending reroute (proptest); seeded simulator determinism; planned deviation produces
   off-route then a reroute request; `no_alloc.rs` still green.

Done when: `cargo test -p navcore` green, benches unchanged within noise.

*Outcome:* done. `Update` carries the `TripState` too (an `Arc` clone, no allocation), so the FFI
layers never need a second lock. The fixture generator reuses `sim::Rng` and the fixtures were
verified byte-identical, so they were **not** regenerated.

### Phase 10 — Approach A catches up (`navcore-ffi`, wrappers)

Keep the pull model, expose the new core pieces so A and B are feature-equivalent.

- `update_location` returns `FullState`; `TripState.reroute: RerouteRequest?` replaces the bool.
- `RouteSimulator` object exported (A can use it too; the demo still replays fixtures for A to
  show the host-driven path).
- Kotlin/Swift `NavigationSession`: `position` and `trip` flows derived from `FullState`;
  `setRoute` after the host fetches; `rerouteRequests` counter kept.
- JVM test and XCTest updated.

Done when: both platform tests replay `route_simple` to arrival; off-route drive triggers one
host-side `setRoute`.

*Outcome:* done. The wrappers take an optional `replanner` closure so the host-driven path is one
line for the demo; without it the request is only surfaced.

### Phase 11 — Approach B FFI (`crates/navcore-boltffi`)

- `cargo install boltffi_cli`; crate-type `["cdylib", "staticlib"]`; `#[data]` records mirroring
  `navcore-ffi`'s; exported object `NavigationController` with `start(route, config)`,
  `update_location(raw)`, `update_location_batch(Vec<RawLocation>)`, `stop()`,
  `add_listener(NavigationListener) -> u32`, `remove_listener(id)`, `set_replanner(ReplanningService)`.
- Host-implemented traits: `NavigationListener { on_position, on_trip, on_error }` and
  `ReplanningService { async fn replan(RerouteRequest) -> Result<Route, NavError> }`.
- Reroute flow: on a request the controller spawns the foreign async `replan`, awaits it on the
  Rust future machinery (no Tokio), installs the route, emits `on_trip`. The core `Mutex` is not
  held across the await.
- `RouteSimulator` exported with the same methods as in A.
- First task is a one-hour spike proving object export, host callbacks and async in the installed
  BoltFFI version. If any is missing, record it in the README as a finding and fall back to a
  `drain_events()` pull for that piece, clearly labelled.
- `scripts/build-boltffi-android.sh` and `build-boltffi-ios.sh` wrapping `boltffi pack`.

Done when: a Rust integration test drives `route_simple` through the controller with an in-process
listener and a fake replanner; `boltffi pack` produces Kotlin and Swift bindings.

*Outcome:* done, no fallback needed. BoltFFI 0.31 supports object export, `Arc<dyn Trait>` host
callbacks and `#[async_trait]` host-implemented async methods. Rust awaits the host's replan on a
helper thread with a 20-line `block_on` (thread-park waker); the host completes the future through
BoltFFI's continuation callback, so no async runtime is linked. Findings worth knowing:
`#[error]` clashes with thiserror's helper attribute (use `#[boltffi::error]`), the generated Kotlin
goes in a dedicated `generated/` folder because the C JNI glue lands next to it, the Apple
`bundled` layout puts generated Swift and the wrapper in one module (`NavCorePush`), and the
`.so` ships unstripped (~6 MB) until AGP strips it (~0.6 MB).

### Phase 12 — Android: `navsdk-push` module and the two-approach demo

- `android/navsdk-push`: BoltFFI output +
  `NavigationSessionPush(route, config, replanner: suspend (RerouteRequest) -> Route)`
  exposing `position: StateFlow`, `trip: StateFlow`, `errors`. Listener callbacks hop to a
  single-threaded dispatcher. JVM test via the desktop JNI library.
- `android/demo`: one Compose app with three tabs.
    - **Pull**: `NavigationSession` (A), fixture replay at 1 Hz, host-side reroute.
    - **Push**: `NavigationSessionPush` (B), Rust simulator or fixture, core-side reroute.
    - **Both**: the same fixture fed to both sessions in lockstep; two cards, one per approach, plus a
      live metrics strip: boundary µs per call (avg / p99), instruction-card recompositions, state
      updates delivered, reroutes, allocations per fix (via `Debug.getNativeHeapAllocatedSize`
      delta), and a "diverged" flag if snapped positions differ by > 0.5 m.
- Instrumented test: replay to arrival on both tabs; "Both" mode never diverges.

Done when: `./gradlew :navsdk:assembleRelease :navsdk-push:assembleRelease :demo:assembleDebug`
succeeds and the instrumented test passes on an emulator.

*Outcome:* done; 5 instrumented tests pass on a physical device. The allocations-per-fix metric
was dropped from the strip (too noisy on ART to mean anything); the strip shows boundary µs
(avg / p99 / max), position and trip update counts, instruction-card recompositions, reroutes,
routes installed and the divergence flag.

### Phase 13 — iOS: `NavSDKPush` package and demo parity

- `ios/NavSDKPush`: BoltFFI XCFramework + `@MainActor NavigationSessionPush: ObservableObject`
  with `@Published position`, `@Published trip`, an `async throws` replanner closure.
- `ios/Demo`: segmented control Pull / Push / Both with the same cards and metrics strip.
- XCTest: replay to arrival on both; "Both" never diverges.

Done when: the simulator build and tests pass using a UDID destination as in `CLAUDE.md`.

*Outcome:* done; 14 simulator tests across the demo and both packages. Two bare-library
XCFrameworks in one app both copy `module.modulemap` into the same `include/` directory and Xcode
fails with "Multiple commands produce"; `scripts/build-ios.sh` now packages the UniFFI slices as
static frameworks, which keep their module maps to themselves.

### Phase 14 — Measurements and write-up

- `scripts/bench-boundary.sh`: 10 000 `update_location` calls through A and through B on the JVM
  and in XCTest; report avg and p99 µs per call, plus the same loop with a 2 000-vertex route to
  show record-size sensitivity.
- README gets a "Pull vs Push" section: the two tables from Section 1 with measured columns (boundary cost,
  AAR/XCFramework size delta, recompositions per replay, lines of wrapper code,
  lines of generated code) and a one-paragraph recommendation per use case (GPS-only at 1 Hz vs
  IMU at 100 Hz, single-platform vs multi-platform, team familiarity).
- CI: add `navsdk-push` and `NavSDKPush` to the android and ios jobs; cache the BoltFFI CLI.

Done when: the README tables have numbers, not claims, and CI is green on all three jobs.

*Outcome:* numbers in `README.md`; CI builds both approaches (not yet run on GitHub).

---

## 4. Backlog (either approach)

- Batched IMU input and an INS predictor with tunnel projection along the route.
- GPS integrity (jamming/spoofing) state.
- Provider route parsing in Rust (OSRM first), polyline decoding, congestion segments.
- Better-route polling with accept/decline, route refresh for ETA.
- Session recording/replay in Rust.
- Horizon events (speed cameras, incidents) scoped to the current step.
- Packaging as versioned Maven / SwiftPM artifacts with a platform location adapter module.
