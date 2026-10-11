//! `navcore-boltffi` — BoltFFI layer over [`navcore`]: **Approach B, push**.
//!
//! The host feeds locations and subscribes to a [`NavigationListener`]; the
//! controller calls back with a [`PositionState`] on every accepted fix, a
//! [`TripState`] only when it changed, and a [`SessionState`] on lifecycle
//! events. Rerouting is core-owned: when the navigator issues a request the
//! controller calls the host's [`ReplanningService`], awaits it on a helper
//! thread, installs the returned route and reports the outcome.
//!
//! Like `navcore-ffi`, this crate only converts types and forwards calls: no
//! navigation logic lives here, and no `unwrap`/`expect`. The generated JNI
//! glue is the only `unsafe` code, which is why this crate does not
//! `deny(unsafe_code)`.
//!
//! Indices are `u32` here because the binding layer has no `usize`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use async_trait::async_trait;
use boltffi::*;

// ---------------------------------------------------------------------------
// Records and enums
// ---------------------------------------------------------------------------

/// WGS-84 coordinate in degrees.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lng: f64,
}

/// A raw fix from the platform location provider.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawLocation {
    pub point: GeoPoint,
    /// Horizontal accuracy, metres (1σ).
    pub accuracy_m: Option<f64>,
    /// Ground speed, metres/second.
    pub speed_mps: Option<f64>,
    /// Course over ground, degrees clockwise from true north.
    pub course_deg: Option<f64>,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
}

#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManeuverType {
    Depart,
    TurnLeft,
    TurnRight,
    SlightLeft,
    SlightRight,
    Straight,
    UTurn,
    Arrive,
}

/// One step of a route. The manoeuvre happens at the step's start; the last
/// step is a zero-length `Arrive` step.
#[data]
#[derive(Debug, Clone, PartialEq)]
pub struct RouteStep {
    pub instruction: String,
    pub maneuver: ManeuverType,
    pub start_index: u32,
    pub end_index: u32,
    pub distance_m: f64,
}

#[data]
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub geometry: Vec<GeoPoint>,
    pub steps: Vec<RouteStep>,
}

#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnappedLocation {
    pub point: GeoPoint,
    pub segment_index: u32,
    pub distance_along_route_m: f64,
    pub distance_from_route_m: f64,
    /// Degrees clockwise from north, `[0, 360)`.
    pub bearing_deg: f64,
}

#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripProgress {
    NotStarted,
    Navigating,
    Arrived,
}

/// The part of the state that changes on every accepted fix.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionState {
    pub snapped: SnappedLocation,
    pub distance_to_next_maneuver_m: f64,
    pub distance_remaining_m: f64,
    /// Repeated from `TripState` so position-only consumers can show a badge.
    pub is_off_route: bool,
    /// Repeated from `TripState`.
    pub progress: TripProgress,
}

/// Why the core asks for a new route.
#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerouteReason {
    /// Fetch a route from `origin` to `destination`.
    OffRoute,
    /// The attempt limit was reached; informational.
    GaveUp,
}

/// A request from the core to fetch a new route. Present in `TripState`
/// only on the update that issues it.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerouteRequest {
    pub origin: GeoPoint,
    pub heading_deg: Option<f64>,
    pub destination: GeoPoint,
    /// 1 for the first request since the route was set, then 2, 3, …
    pub attempt: u32,
    pub reason: RerouteReason,
    pub timestamp_ms: u64,
}

/// The part of the state that changes rarely: step advance, off-route
/// transitions, arrival and reroute requests.
#[data]
#[derive(Debug, Clone, PartialEq)]
pub struct TripState {
    pub current_step_index: u32,
    pub step_count: u32,
    pub next_instruction: String,
    pub is_off_route: bool,
    pub progress: TripProgress,
    /// Set only on the update that issues a reroute request.
    pub reroute: Option<RerouteRequest>,
}

/// Lifecycle of a controller.
#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Accepting fixes.
    Navigating,
    /// `stop()` was called; fixes are ignored until `set_route`.
    Stopped,
}

/// The slowest-changing stream: lifecycle, routes installed, replanning.
#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionState {
    pub status: SessionStatus,
    /// Routes installed through `set_route` or a completed replan.
    pub routes_installed: u32,
    /// Reroute requests issued since the route was last set.
    pub reroute_attempts: u32,
    /// True while the host's `ReplanningService` is being awaited.
    pub replan_in_flight: bool,
    /// Replans the host answered with a route.
    pub replans_succeeded: u32,
    /// Replans the host declined or failed.
    pub replans_failed: u32,
}

/// Error classes, for `NavigationListener::on_error`.
#[data]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidRoute,
    StaleLocation,
    ImplausibleLocation,
    InvalidLocation,
    InvalidConfig,
    InvalidJson,
}

/// All tunables. Every field has a default, so hosts can construct it with
/// no arguments and override selectively.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavigatorConfig {
    #[boltffi::default(20.0)]
    pub step_advance_distance_m: f64,
    #[boltffi::default(45.0)]
    pub step_advance_heading_tolerance_deg: f64,
    #[boltffi::default(30.0)]
    pub off_route_distance_m: f64,
    #[boltffi::default(3)]
    pub off_route_min_consecutive: u32,
    #[boltffi::default(2)]
    pub on_route_min_consecutive: u32,
    #[boltffi::default(1.0)]
    pub off_route_min_speed_mps: f64,
    #[boltffi::default(5.0)]
    pub off_route_min_movement_m: f64,
    #[boltffi::default(10.0)]
    pub reroute_grace_period_s: f64,
    #[boltffi::default(50.0)]
    pub suppress_off_route_near_destination_m: f64,
    #[boltffi::default(10.0)]
    pub min_time_between_reroutes_s: f64,
    /// `0` means no limit.
    #[boltffi::default(3)]
    pub max_reroute_attempts: u32,
    #[boltffi::default(15.0)]
    pub arrival_distance_m: f64,
    #[boltffi::default(70.0)]
    pub max_speed_mps: f64,
    #[boltffi::default(1.0)]
    pub process_noise_accel_mps2: f64,
    #[boltffi::default(10.0)]
    pub default_accuracy_m: f64,
    #[boltffi::default(50.0)]
    pub candidate_radius_m: f64,
    #[boltffi::default(150.0)]
    pub widened_candidate_radius_m: f64,
    #[boltffi::default(30.0)]
    pub max_backtrack_m: f64,
    #[boltffi::default(10)]
    pub hmm_window_size: u32,
}

impl Default for NavigatorConfig {
    fn default() -> Self {
        navcore::NavigatorConfig::default().into()
    }
}

/// Tunables for `RouteSimulator`. Every field has a default.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulatorConfig {
    #[boltffi::default(13.9)]
    pub speed_mps: f64,
    #[boltffi::default(1.0)]
    pub fix_interval_s: f64,
    #[boltffi::default(8.0)]
    pub position_sigma_m: f64,
    #[boltffi::default(10.0)]
    pub reported_accuracy_m: f64,
    #[boltffi::default(0.5)]
    pub speed_sigma_mps: f64,
    #[boltffi::default(5.0)]
    pub course_sigma_deg: f64,
    #[boltffi::default(1)]
    pub seed: u64,
    #[boltffi::default(1700000000000)]
    pub start_timestamp_ms: u64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        navcore::SimulatorConfig::default().into()
    }
}

/// Leave the route: from `at_m` along it, drive parallel offset by
/// `lateral_m` (positive = right of travel) for `length_m`, then rejoin.
#[data]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannedDeviation {
    pub at_m: f64,
    pub lateral_m: f64,
    pub length_m: f64,
}

/// Errors surfaced to the host as exceptions.
#[boltffi::error]
#[derive(Debug, Clone, PartialEq)]
pub enum NavError {
    InvalidRoute {
        reason: String,
    },
    StaleLocation {
        timestamp_ms: u64,
        last_timestamp_ms: u64,
    },
    ImplausibleLocation {
        implied_speed_mps: f64,
        max_speed_mps: f64,
    },
    InvalidLocation,
    InvalidConfig {
        reason: String,
    },
    InvalidJson {
        reason: String,
    },
}

impl std::fmt::Display for NavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavError::InvalidRoute { reason } => write!(f, "invalid route: {reason}"),
            NavError::StaleLocation {
                timestamp_ms,
                last_timestamp_ms,
            } => write!(
                f,
                "stale location: {timestamp_ms} is not after {last_timestamp_ms}"
            ),
            NavError::ImplausibleLocation {
                implied_speed_mps,
                max_speed_mps,
            } => write!(
                f,
                "implausible location: {implied_speed_mps} m/s exceeds {max_speed_mps} m/s"
            ),
            NavError::InvalidLocation => write!(f, "invalid location coordinate"),
            NavError::InvalidConfig { reason } => write!(f, "invalid config: {reason}"),
            NavError::InvalidJson { reason } => write!(f, "invalid json: {reason}"),
        }
    }
}

impl std::error::Error for NavError {}

// ---------------------------------------------------------------------------
// Host-implemented callbacks
// ---------------------------------------------------------------------------

/// Receives state from the controller. Called on the thread that fed the
/// fix (`update_location`), or on the replanning helper thread for session
/// updates caused by a replan. Implementations must not block.
#[export]
pub trait NavigationListener: Send + Sync {
    /// Every accepted fix.
    fn on_position(&self, position: PositionState);
    /// Only when the trip state changed or carries a reroute request.
    fn on_trip(&self, trip: TripState);
    /// Lifecycle: start, stop, route installed, replan started/finished.
    fn on_session(&self, session: SessionState);
    /// A rejected fix or a failed route install. State is unchanged.
    fn on_error(&self, kind: ErrorKind, message: String);
}

/// Fetches routes for the controller. Return `None` to decline; the core
/// retries after `min_time_between_reroutes_s`, up to `max_reroute_attempts`.
#[async_trait]
#[export]
pub trait ReplanningService: Send + Sync {
    async fn replan(&self, request: RerouteRequest) -> Option<Route>;
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Parse and validate a route from JSON (`{"geometry": [...], "steps": [...]}`).
#[export]
pub fn route_from_json(json: &str) -> Result<Route, NavError> {
    navcore::Route::from_json(json)
        .map(Route::from)
        .map_err(NavError::from)
}

/// The default configuration, for hosts that want to inspect or tweak it.
#[export]
pub fn default_navigator_config() -> NavigatorConfig {
    NavigatorConfig::default()
}

/// The default simulator configuration.
#[export]
pub fn default_simulator_config() -> SimulatorConfig {
    SimulatorConfig::default()
}

/// Core crate version, for demos to prove the native library is linked.
#[export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------------------
// NavigationController
// ---------------------------------------------------------------------------

struct Shared {
    nav: Mutex<navcore::Navigator>,
    listeners: Mutex<Vec<(u32, Arc<dyn NavigationListener>)>>,
    next_listener_id: AtomicU32,
    replanner: Mutex<Option<Arc<dyn ReplanningService>>>,
    session: Mutex<SessionState>,
}

/// Lock, recovering from poisoning: staying alive matters more than a
/// half-applied update after a panic elsewhere.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn listeners(&self) -> Vec<Arc<dyn NavigationListener>> {
        lock(&self.listeners)
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect()
    }

    fn emit_session(&self) {
        let snapshot = *lock(&self.session);
        for l in self.listeners() {
            l.on_session(snapshot);
        }
    }

    fn emit_error(&self, error: navcore::NavError) {
        let kind = ErrorKind::from(&error);
        let message = error.to_string();
        for l in self.listeners() {
            l.on_error(kind, message.clone());
        }
    }

    /// Feed one fix and fan the result out. Listeners are called with no
    /// lock held so they may call back into the controller.
    fn update(&self, raw: RawLocation) {
        if lock(&self.session).status == SessionStatus::Stopped {
            return;
        }
        let result = lock(&self.nav).update_location(raw.into());
        match result {
            Ok(update) => {
                let position = PositionState::from(update.position);
                let trip = if update.trip_changed {
                    Some(TripState::from(update.trip.clone()))
                } else {
                    None
                };
                for l in self.listeners() {
                    l.on_position(position);
                    if let Some(t) = &trip {
                        l.on_trip(t.clone());
                    }
                }
                if let Some(request) = update.trip.reroute {
                    self.on_reroute(request);
                }
            }
            Err(e) => self.emit_error(e),
        }
    }

    fn on_reroute(&self, request: navcore::RerouteRequest) {
        {
            let mut s = lock(&self.session);
            s.reroute_attempts = request.attempt;
        }
        if request.reason != navcore::RerouteReason::OffRoute {
            self.emit_session();
        }
    }

    fn install_route(&self, route: navcore::Route) -> Result<(), navcore::NavError> {
        lock(&self.nav).set_route(route)?;
        let mut s = lock(&self.session);
        s.routes_installed += 1;
        s.reroute_attempts = 0;
        s.status = SessionStatus::Navigating;
        Ok(())
    }
}

/// Thread-safe navigation controller: the host pushes fixes in and gets
/// state back through listeners. Hosts may call from any thread.
pub struct NavigationController {
    shared: Arc<Shared>,
}

impl NavigationController {
    /// Start the host's replanner for `request` on a helper thread, unless
    /// one is already running. The helper installs the returned route and
    /// reports through the session stream.
    fn start_replan(&self, request: navcore::RerouteRequest) {
        let Some(replanner) = lock(&self.shared.replanner).clone() else {
            return;
        };
        {
            let mut s = lock(&self.shared.session);
            if s.replan_in_flight {
                return;
            }
            s.replan_in_flight = true;
        }
        self.shared.emit_session();
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name("navcore-replan".into())
            .spawn(move || {
                let route = block_on(replanner.replan(request.into()));
                let installed = match route {
                    Some(route) => match shared.install_route(route.into()) {
                        Ok(()) => true,
                        Err(e) => {
                            shared.emit_error(e);
                            false
                        }
                    },
                    None => false,
                };
                {
                    let mut s = lock(&shared.session);
                    s.replan_in_flight = false;
                    if installed {
                        s.replans_succeeded += 1;
                    } else {
                        s.replans_failed += 1;
                    }
                }
                shared.emit_session();
            });
        if spawned.is_err() {
            let mut s = lock(&self.shared.session);
            s.replan_in_flight = false;
            s.replans_failed += 1;
        }
    }
}

#[export]
impl NavigationController {
    /// Build a controller for `route`. Fails on an invalid route or config.
    pub fn new(route: Route, config: NavigatorConfig) -> Result<Self, NavError> {
        let nav = navcore::Navigator::new(route.into(), config.into())?;
        Ok(Self {
            shared: Arc::new(Shared {
                nav: Mutex::new(nav),
                listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU32::new(1),
                replanner: Mutex::new(None),
                session: Mutex::new(SessionState {
                    status: SessionStatus::Navigating,
                    routes_installed: 0,
                    reroute_attempts: 0,
                    replan_in_flight: false,
                    replans_succeeded: 0,
                    replans_failed: 0,
                }),
            }),
        })
    }

    /// Subscribe. The listener immediately receives the current session
    /// state, then every later update. Returns an id for `remove_listener`.
    pub fn add_listener(&self, listener: Arc<dyn NavigationListener>) -> u32 {
        let id = self.shared.next_listener_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.listeners).push((id, Arc::clone(&listener)));
        listener.on_session(*lock(&self.shared.session));
        id
    }

    /// Unsubscribe. Unknown ids are ignored.
    pub fn remove_listener(&self, id: u32) {
        lock(&self.shared.listeners).retain(|(i, _)| *i != id);
    }

    /// Install the host's route fetcher. Without one, reroute requests are
    /// only reported through the trip stream.
    pub fn set_replanner(&self, replanner: Arc<dyn ReplanningService>) {
        *lock(&self.shared.replanner) = Some(replanner);
    }

    /// Remove the route fetcher.
    pub fn clear_replanner(&self) {
        *lock(&self.shared.replanner) = None;
    }

    /// Feed one fix. Results arrive through the listeners; a rejected fix
    /// produces `on_error` and leaves the state untouched.
    pub fn update_location(&self, raw: RawLocation) {
        let reroute = {
            // `update` fans out position/trip; the reroute decision needs
            // `&self` to spawn, so read it back from the core afterwards.
            self.shared.update(raw);
            lock(&self.shared.nav)
                .trip()
                .and_then(|t| t.reroute)
                .filter(|r| r.reason == navcore::RerouteReason::OffRoute)
        };
        if let Some(request) = reroute {
            self.start_replan(request);
        }
    }

    /// Feed several fixes in one boundary crossing, in order.
    pub fn update_location_batch(&self, fixes: Vec<RawLocation>) {
        for raw in fixes {
            self.update_location(raw);
        }
    }

    /// Replace the route. Answers any pending reroute request and resumes
    /// after `stop()`.
    pub fn set_route(&self, route: Route) -> Result<(), NavError> {
        self.shared.install_route(route.into())?;
        self.shared.emit_session();
        Ok(())
    }

    /// Stop accepting fixes until the next `set_route`.
    pub fn stop(&self) {
        lock(&self.shared.session).status = SessionStatus::Stopped;
        self.shared.emit_session();
    }

    /// Latest per-fix state (pull accessor, mainly for tests).
    pub fn position(&self) -> Option<PositionState> {
        lock(&self.shared.nav)
            .position()
            .copied()
            .map(PositionState::from)
    }

    /// Latest slow-changing state (pull accessor, mainly for tests).
    pub fn trip(&self) -> Option<TripState> {
        lock(&self.shared.nav).trip().cloned().map(TripState::from)
    }

    /// Current session state.
    pub fn session(&self) -> SessionState {
        *lock(&self.shared.session)
    }

    /// The active route.
    pub fn route(&self) -> Route {
        lock(&self.shared.nav).route().clone().into()
    }
}

/// Drive a future to completion on the current thread. The host completes
/// `ReplanningService` futures through the binding layer's continuation
/// callback, which wakes this thread; no async runtime is needed.
fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    // SAFETY-free pinning: `future` lives on this stack frame for the whole
    // loop and is never moved after being pinned.
    let mut future = std::pin::pin!(future);
    loop {
        match Pin::as_mut(&mut future).poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => thread::park(),
        }
    }
}

// ---------------------------------------------------------------------------
// RouteSimulator
// ---------------------------------------------------------------------------

/// Thread-safe handle over the core route simulator. Same seed, same fixes,
/// on every platform.
pub struct RouteSimulator {
    inner: Mutex<navcore::RouteSimulator>,
}

#[export]
impl RouteSimulator {
    pub fn new(route: Route, config: SimulatorConfig) -> Result<Self, NavError> {
        let sim = navcore::RouteSimulator::new(&route.into(), config.into())?;
        Ok(Self {
            inner: Mutex::new(sim),
        })
    }

    /// Advance by the configured fix interval and emit a fix.
    pub fn next_fix(&self) -> RawLocation {
        lock(&self.inner).next_fix().into()
    }

    /// Advance by `dt_s` seconds and emit a fix.
    pub fn advance(&self, dt_s: f64) -> RawLocation {
        lock(&self.inner).advance(dt_s).into()
    }

    pub fn set_target_speed(&self, speed_mps: f64) {
        lock(&self.inner).set_target_speed(speed_mps);
    }

    pub fn target_speed_mps(&self) -> f64 {
        lock(&self.inner).target_speed_mps()
    }

    pub fn set_planned_deviation(&self, deviation: PlannedDeviation) {
        lock(&self.inner).set_planned_deviation(deviation.into());
    }

    pub fn clear_planned_deviation(&self) {
        lock(&self.inner).clear_planned_deviation();
    }

    /// Multiply position noise and reported accuracy by `sigma_multiplier` (≥ 1).
    pub fn start_signal_degradation(&self, sigma_multiplier: f64) {
        lock(&self.inner).start_signal_degradation(sigma_multiplier);
    }

    pub fn stop_signal_degradation(&self) {
        lock(&self.inner).stop_signal_degradation();
    }

    pub fn is_signal_degraded(&self) -> bool {
        lock(&self.inner).is_signal_degraded()
    }

    /// True once the vehicle has reached the end of the route.
    pub fn is_finished(&self) -> bool {
        lock(&self.inner).is_finished()
    }

    pub fn distance_along_m(&self) -> f64 {
        lock(&self.inner).distance_along_m()
    }

    pub fn route_length_m(&self) -> f64 {
        lock(&self.inner).route_length_m()
    }

    /// Fixes emitted since the last reset.
    pub fn fixes_emitted(&self) -> u64 {
        lock(&self.inner).fixes_emitted()
    }

    /// Noise-free position at the last emitted fix.
    pub fn truth(&self) -> GeoPoint {
        lock(&self.inner).truth().into()
    }

    /// Rewind to the start with the original seed.
    pub fn reset(&self) {
        lock(&self.inner).reset();
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<GeoPoint> for navcore::GeoPoint {
    fn from(p: GeoPoint) -> Self {
        navcore::GeoPoint::new(p.lat, p.lng)
    }
}

impl From<navcore::GeoPoint> for GeoPoint {
    fn from(p: navcore::GeoPoint) -> Self {
        GeoPoint {
            lat: p.lat,
            lng: p.lng,
        }
    }
}

impl From<RawLocation> for navcore::RawLocation {
    fn from(r: RawLocation) -> Self {
        navcore::RawLocation {
            point: r.point.into(),
            accuracy_m: r.accuracy_m,
            speed_mps: r.speed_mps,
            course_deg: r.course_deg,
            timestamp_ms: r.timestamp_ms,
        }
    }
}

impl From<navcore::RawLocation> for RawLocation {
    fn from(r: navcore::RawLocation) -> Self {
        RawLocation {
            point: r.point.into(),
            accuracy_m: r.accuracy_m,
            speed_mps: r.speed_mps,
            course_deg: r.course_deg,
            timestamp_ms: r.timestamp_ms,
        }
    }
}

impl From<ManeuverType> for navcore::ManeuverType {
    fn from(m: ManeuverType) -> Self {
        match m {
            ManeuverType::Depart => navcore::ManeuverType::Depart,
            ManeuverType::TurnLeft => navcore::ManeuverType::TurnLeft,
            ManeuverType::TurnRight => navcore::ManeuverType::TurnRight,
            ManeuverType::SlightLeft => navcore::ManeuverType::SlightLeft,
            ManeuverType::SlightRight => navcore::ManeuverType::SlightRight,
            ManeuverType::Straight => navcore::ManeuverType::Straight,
            ManeuverType::UTurn => navcore::ManeuverType::UTurn,
            ManeuverType::Arrive => navcore::ManeuverType::Arrive,
        }
    }
}

impl From<navcore::ManeuverType> for ManeuverType {
    fn from(m: navcore::ManeuverType) -> Self {
        match m {
            navcore::ManeuverType::Depart => ManeuverType::Depart,
            navcore::ManeuverType::TurnLeft => ManeuverType::TurnLeft,
            navcore::ManeuverType::TurnRight => ManeuverType::TurnRight,
            navcore::ManeuverType::SlightLeft => ManeuverType::SlightLeft,
            navcore::ManeuverType::SlightRight => ManeuverType::SlightRight,
            navcore::ManeuverType::Straight => ManeuverType::Straight,
            navcore::ManeuverType::UTurn => ManeuverType::UTurn,
            navcore::ManeuverType::Arrive => ManeuverType::Arrive,
        }
    }
}

impl From<RouteStep> for navcore::RouteStep {
    fn from(s: RouteStep) -> Self {
        navcore::RouteStep {
            instruction: s.instruction,
            maneuver: s.maneuver.into(),
            start_index: s.start_index as usize,
            end_index: s.end_index as usize,
            distance_m: s.distance_m,
        }
    }
}

impl From<navcore::RouteStep> for RouteStep {
    fn from(s: navcore::RouteStep) -> Self {
        RouteStep {
            instruction: s.instruction,
            maneuver: s.maneuver.into(),
            start_index: index_to_u32(s.start_index),
            end_index: index_to_u32(s.end_index),
            distance_m: s.distance_m,
        }
    }
}

impl From<Route> for navcore::Route {
    fn from(r: Route) -> Self {
        navcore::Route {
            geometry: r.geometry.into_iter().map(Into::into).collect(),
            steps: r.steps.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<navcore::Route> for Route {
    fn from(r: navcore::Route) -> Self {
        Route {
            geometry: r.geometry.into_iter().map(Into::into).collect(),
            steps: r.steps.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<navcore::SnappedLocation> for SnappedLocation {
    fn from(s: navcore::SnappedLocation) -> Self {
        SnappedLocation {
            point: s.point.into(),
            segment_index: index_to_u32(s.segment_index),
            distance_along_route_m: s.distance_along_route_m,
            distance_from_route_m: s.distance_from_route_m,
            bearing_deg: s.bearing_deg,
        }
    }
}

impl From<navcore::TripProgress> for TripProgress {
    fn from(p: navcore::TripProgress) -> Self {
        match p {
            navcore::TripProgress::NotStarted => TripProgress::NotStarted,
            navcore::TripProgress::Navigating => TripProgress::Navigating,
            navcore::TripProgress::Arrived => TripProgress::Arrived,
        }
    }
}

impl From<navcore::PositionState> for PositionState {
    fn from(s: navcore::PositionState) -> Self {
        PositionState {
            snapped: s.snapped.into(),
            distance_to_next_maneuver_m: s.distance_to_next_maneuver_m,
            distance_remaining_m: s.distance_remaining_m,
            is_off_route: s.is_off_route,
            progress: s.progress.into(),
        }
    }
}

impl From<navcore::RerouteReason> for RerouteReason {
    fn from(r: navcore::RerouteReason) -> Self {
        match r {
            navcore::RerouteReason::OffRoute => RerouteReason::OffRoute,
            navcore::RerouteReason::GaveUp => RerouteReason::GaveUp,
        }
    }
}

impl From<navcore::RerouteRequest> for RerouteRequest {
    fn from(r: navcore::RerouteRequest) -> Self {
        RerouteRequest {
            origin: r.origin.into(),
            heading_deg: r.heading_deg,
            destination: r.destination.into(),
            attempt: r.attempt,
            reason: r.reason.into(),
            timestamp_ms: r.timestamp_ms,
        }
    }
}

impl From<navcore::TripState> for TripState {
    fn from(s: navcore::TripState) -> Self {
        TripState {
            current_step_index: index_to_u32(s.current_step_index),
            step_count: index_to_u32(s.step_count),
            next_instruction: s.next_instruction.to_string(),
            is_off_route: s.is_off_route,
            progress: s.progress.into(),
            reroute: s.reroute.map(RerouteRequest::from),
        }
    }
}

impl From<NavigatorConfig> for navcore::NavigatorConfig {
    fn from(c: NavigatorConfig) -> Self {
        navcore::NavigatorConfig {
            step_advance_distance_m: c.step_advance_distance_m,
            step_advance_heading_tolerance_deg: c.step_advance_heading_tolerance_deg,
            off_route_distance_m: c.off_route_distance_m,
            off_route_min_consecutive: c.off_route_min_consecutive,
            on_route_min_consecutive: c.on_route_min_consecutive,
            off_route_min_speed_mps: c.off_route_min_speed_mps,
            off_route_min_movement_m: c.off_route_min_movement_m,
            reroute_grace_period_s: c.reroute_grace_period_s,
            suppress_off_route_near_destination_m: c.suppress_off_route_near_destination_m,
            min_time_between_reroutes_s: c.min_time_between_reroutes_s,
            max_reroute_attempts: c.max_reroute_attempts,
            arrival_distance_m: c.arrival_distance_m,
            max_speed_mps: c.max_speed_mps,
            process_noise_accel_mps2: c.process_noise_accel_mps2,
            default_accuracy_m: c.default_accuracy_m,
            candidate_radius_m: c.candidate_radius_m,
            widened_candidate_radius_m: c.widened_candidate_radius_m,
            max_backtrack_m: c.max_backtrack_m,
            hmm_window_size: c.hmm_window_size,
        }
    }
}

impl From<navcore::NavigatorConfig> for NavigatorConfig {
    fn from(c: navcore::NavigatorConfig) -> Self {
        NavigatorConfig {
            step_advance_distance_m: c.step_advance_distance_m,
            step_advance_heading_tolerance_deg: c.step_advance_heading_tolerance_deg,
            off_route_distance_m: c.off_route_distance_m,
            off_route_min_consecutive: c.off_route_min_consecutive,
            on_route_min_consecutive: c.on_route_min_consecutive,
            off_route_min_speed_mps: c.off_route_min_speed_mps,
            off_route_min_movement_m: c.off_route_min_movement_m,
            reroute_grace_period_s: c.reroute_grace_period_s,
            suppress_off_route_near_destination_m: c.suppress_off_route_near_destination_m,
            min_time_between_reroutes_s: c.min_time_between_reroutes_s,
            max_reroute_attempts: c.max_reroute_attempts,
            arrival_distance_m: c.arrival_distance_m,
            max_speed_mps: c.max_speed_mps,
            process_noise_accel_mps2: c.process_noise_accel_mps2,
            default_accuracy_m: c.default_accuracy_m,
            candidate_radius_m: c.candidate_radius_m,
            widened_candidate_radius_m: c.widened_candidate_radius_m,
            max_backtrack_m: c.max_backtrack_m,
            hmm_window_size: c.hmm_window_size,
        }
    }
}

impl From<SimulatorConfig> for navcore::SimulatorConfig {
    fn from(c: SimulatorConfig) -> Self {
        navcore::SimulatorConfig {
            speed_mps: c.speed_mps,
            fix_interval_s: c.fix_interval_s,
            position_sigma_m: c.position_sigma_m,
            reported_accuracy_m: c.reported_accuracy_m,
            speed_sigma_mps: c.speed_sigma_mps,
            course_sigma_deg: c.course_sigma_deg,
            seed: c.seed,
            start_timestamp_ms: c.start_timestamp_ms,
        }
    }
}

impl From<navcore::SimulatorConfig> for SimulatorConfig {
    fn from(c: navcore::SimulatorConfig) -> Self {
        SimulatorConfig {
            speed_mps: c.speed_mps,
            fix_interval_s: c.fix_interval_s,
            position_sigma_m: c.position_sigma_m,
            reported_accuracy_m: c.reported_accuracy_m,
            speed_sigma_mps: c.speed_sigma_mps,
            course_sigma_deg: c.course_sigma_deg,
            seed: c.seed,
            start_timestamp_ms: c.start_timestamp_ms,
        }
    }
}

impl From<PlannedDeviation> for navcore::PlannedDeviation {
    fn from(d: PlannedDeviation) -> Self {
        navcore::PlannedDeviation {
            at_m: d.at_m,
            lateral_m: d.lateral_m,
            length_m: d.length_m,
        }
    }
}

impl From<navcore::NavError> for NavError {
    fn from(e: navcore::NavError) -> Self {
        match e {
            navcore::NavError::InvalidRoute(reason) => NavError::InvalidRoute { reason },
            navcore::NavError::StaleLocation {
                timestamp_ms,
                last_timestamp_ms,
            } => NavError::StaleLocation {
                timestamp_ms,
                last_timestamp_ms,
            },
            navcore::NavError::ImplausibleLocation {
                implied_speed_mps,
                max_speed_mps,
            } => NavError::ImplausibleLocation {
                implied_speed_mps,
                max_speed_mps,
            },
            navcore::NavError::InvalidLocation => NavError::InvalidLocation,
            navcore::NavError::InvalidConfig(reason) => NavError::InvalidConfig { reason },
            navcore::NavError::InvalidJson(reason) => NavError::InvalidJson { reason },
        }
    }
}

impl From<&navcore::NavError> for ErrorKind {
    fn from(e: &navcore::NavError) -> Self {
        match e {
            navcore::NavError::InvalidRoute(_) => ErrorKind::InvalidRoute,
            navcore::NavError::StaleLocation { .. } => ErrorKind::StaleLocation,
            navcore::NavError::ImplausibleLocation { .. } => ErrorKind::ImplausibleLocation,
            navcore::NavError::InvalidLocation => ErrorKind::InvalidLocation,
            navcore::NavError::InvalidConfig(_) => ErrorKind::InvalidConfig,
            navcore::NavError::InvalidJson(_) => ErrorKind::InvalidJson,
        }
    }
}

/// Route indices never approach `u32::MAX`; saturate rather than wrap so the
/// conversion can never be silently wrong.
fn index_to_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}
