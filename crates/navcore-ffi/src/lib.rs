//! `navcore-ffi` — UniFFI layer over [`navcore`].
//!
//! This crate only converts types and forwards calls. No logic lives here,
//! and no `unwrap`/`expect`: every fallible operation returns
//! `Result<_, NavError>` so nothing can panic across the FFI boundary.
//!
//! Indices are `u32` here because UniFFI has no `usize`.

use std::sync::{Arc, Mutex};

uniffi::setup_scaffolding!("navcore");

// ---------------------------------------------------------------------------
// Records and enums
// ---------------------------------------------------------------------------

/// WGS-84 coordinate in degrees.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct GeoPoint {
    pub lat: f64,
    pub lng: f64,
}

/// A raw fix from the platform location provider.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct RawLocation {
    pub point: GeoPoint,
    /// Horizontal accuracy, metres (1σ).
    #[uniffi(default = None)]
    pub accuracy_m: Option<f64>,
    /// Ground speed, metres/second.
    #[uniffi(default = None)]
    pub speed_mps: Option<f64>,
    /// Course over ground, degrees clockwise from true north.
    #[uniffi(default = None)]
    pub course_deg: Option<f64>,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RouteStep {
    pub instruction: String,
    pub maneuver: ManeuverType,
    pub start_index: u32,
    pub end_index: u32,
    pub distance_m: f64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Route {
    pub geometry: Vec<GeoPoint>,
    pub steps: Vec<RouteStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SnappedLocation {
    pub point: GeoPoint,
    pub segment_index: u32,
    pub distance_along_route_m: f64,
    pub distance_from_route_m: f64,
    /// Degrees clockwise from north, `[0, 360)`.
    pub bearing_deg: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TripProgress {
    NotStarted,
    Navigating,
    Arrived,
}

/// The part of the state that changes on every accepted fix.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct PositionState {
    pub snapped: SnappedLocation,
    pub distance_to_next_maneuver_m: f64,
    pub distance_remaining_m: f64,
    /// Filtered ground speed, metres/second.
    pub speed_mps: f64,
    /// Seconds to the destination over a smoothed average speed. `None` until
    /// the trip has moved for a few seconds; `0` once arrived.
    pub eta_s: Option<f64>,
    /// Repeated from `TripState` so position-only consumers can show a badge.
    pub is_off_route: bool,
    /// Repeated from `TripState`.
    pub progress: TripProgress,
}

/// Why the core asks for a new route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RerouteReason {
    /// Fetch a route from `origin` to `destination` and call `set_route`.
    OffRoute,
    /// The attempt limit was reached; informational, do not fetch again.
    GaveUp,
}

/// A request from the core to fetch a new route. Present in `TripState`
/// only on the update that issues it.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct RerouteRequest {
    pub origin: GeoPoint,
    #[uniffi(default = None)]
    pub heading_deg: Option<f64>,
    pub destination: GeoPoint,
    /// 1 for the first request since the route was set, then 2, 3, …
    pub attempt: u32,
    pub reason: RerouteReason,
    pub timestamp_ms: u64,
}

/// The part of the state that changes rarely: step advance, off-route
/// transitions, arrival and reroute requests.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TripState {
    pub current_step_index: u32,
    pub step_count: u32,
    pub next_instruction: String,
    pub is_off_route: bool,
    pub progress: TripProgress,
    /// Set only on the update that issues a reroute request.
    #[uniffi(default = None)]
    pub reroute: Option<RerouteRequest>,
}

/// Both halves together: what the pull-style `update_location` returns.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FullState {
    pub position: PositionState,
    pub trip: TripState,
    /// True when `trip` differs from the previous update's trip state or
    /// carries a reroute request.
    pub trip_changed: bool,
}

/// All tunables. Every field has a default, so hosts can construct it with
/// no arguments and override selectively.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct NavigatorConfig {
    #[uniffi(default = 20.0)]
    pub step_advance_distance_m: f64,
    #[uniffi(default = 45.0)]
    pub step_advance_heading_tolerance_deg: f64,
    #[uniffi(default = 30.0)]
    pub off_route_distance_m: f64,
    #[uniffi(default = 3)]
    pub off_route_min_consecutive: u32,
    #[uniffi(default = 2)]
    pub on_route_min_consecutive: u32,
    #[uniffi(default = 1.0)]
    pub off_route_min_speed_mps: f64,
    #[uniffi(default = 5.0)]
    pub off_route_min_movement_m: f64,
    #[uniffi(default = 10.0)]
    pub reroute_grace_period_s: f64,
    #[uniffi(default = 50.0)]
    pub suppress_off_route_near_destination_m: f64,
    #[uniffi(default = 10.0)]
    pub min_time_between_reroutes_s: f64,
    /// `0` means no limit.
    #[uniffi(default = 3)]
    pub max_reroute_attempts: u32,
    #[uniffi(default = 15.0)]
    pub arrival_distance_m: f64,
    #[uniffi(default = 70.0)]
    pub max_speed_mps: f64,
    #[uniffi(default = 1.0)]
    pub process_noise_accel_mps2: f64,
    #[uniffi(default = 10.0)]
    pub default_accuracy_m: f64,
    #[uniffi(default = 50.0)]
    pub candidate_radius_m: f64,
    #[uniffi(default = 150.0)]
    pub widened_candidate_radius_m: f64,
    #[uniffi(default = 30.0)]
    pub max_backtrack_m: f64,
    #[uniffi(default = 10)]
    pub hmm_window_size: u32,
}

impl Default for NavigatorConfig {
    fn default() -> Self {
        navcore::NavigatorConfig::default().into()
    }
}

/// Tunables for `RouteSimulator`. Every field has a default.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SimulatorConfig {
    #[uniffi(default = 13.9)]
    pub speed_mps: f64,
    #[uniffi(default = 1.0)]
    pub fix_interval_s: f64,
    #[uniffi(default = 8.0)]
    pub position_sigma_m: f64,
    #[uniffi(default = 10.0)]
    pub reported_accuracy_m: f64,
    #[uniffi(default = 0.5)]
    pub speed_sigma_mps: f64,
    #[uniffi(default = 5.0)]
    pub course_sigma_deg: f64,
    #[uniffi(default = 1)]
    pub seed: u64,
    #[uniffi(default = 1700000000000)]
    pub start_timestamp_ms: u64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        navcore::SimulatorConfig::default().into()
    }
}

/// Leave the route: from `at_m` along it, drive parallel offset by
/// `lateral_m` (positive = right of travel) for `length_m`, then rejoin.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct PlannedDeviation {
    pub at_m: f64,
    pub lateral_m: f64,
    pub length_m: f64,
}

/// Errors surfaced to the host as exceptions.
#[derive(Debug, Clone, PartialEq, thiserror::Error, uniffi::Error)]
pub enum NavError {
    #[error("invalid route: {reason}")]
    InvalidRoute { reason: String },
    #[error("stale location: {timestamp_ms} is not after {last_timestamp_ms}")]
    StaleLocation {
        timestamp_ms: u64,
        last_timestamp_ms: u64,
    },
    #[error("implausible location: {implied_speed_mps} m/s exceeds {max_speed_mps} m/s")]
    ImplausibleLocation {
        implied_speed_mps: f64,
        max_speed_mps: f64,
    },
    #[error("invalid location coordinate")]
    InvalidLocation,
    #[error("invalid config: {reason}")]
    InvalidConfig { reason: String },
    #[error("invalid json: {reason}")]
    InvalidJson { reason: String },
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Parse and validate a route from JSON (`{"geometry": [...], "steps": [...]}`).
/// Lets demo apps load fixtures without building routes field by field.
#[uniffi::export]
pub fn route_from_json(json: &str) -> Result<Route, NavError> {
    navcore::Route::from_json(json)
        .map(Route::from)
        .map_err(NavError::from)
}

/// The default configuration, for hosts that want to inspect or tweak it.
#[uniffi::export]
pub fn default_navigator_config() -> NavigatorConfig {
    NavigatorConfig::default()
}

/// The default simulator configuration.
#[uniffi::export]
pub fn default_simulator_config() -> SimulatorConfig {
    SimulatorConfig::default()
}

/// A zigzag walking course anchored at `origin`: three legs of `leg_m` metres
/// starting on `heading_deg`, a right turn then a left turn, arrival `√5·leg_m`
/// from the start. Lets a tester exercise the navigator on foot with no map
/// service.
#[uniffi::export]
pub fn walk_course(origin: GeoPoint, heading_deg: f64, leg_m: f64) -> Result<Route, NavError> {
    navcore::courses::walk_course(origin.into(), heading_deg, leg_m)
        .map(Route::from)
        .map_err(NavError::from)
}

/// A straight route from `from` to `to`, the stand-in reroute a host can
/// install when the navigator asks for one.
#[uniffi::export]
pub fn direct_route(from: GeoPoint, to: GeoPoint) -> Result<Route, NavError> {
    navcore::courses::direct_route(from.into(), to.into())
        .map(Route::from)
        .map_err(NavError::from)
}

/// Core crate version, for demos to prove the native library is linked.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------------------
// Navigator object
// ---------------------------------------------------------------------------

/// Thread-safe handle over the core navigator. Hosts may call from any thread.
#[derive(uniffi::Object)]
pub struct Navigator {
    inner: Mutex<navcore::Navigator>,
}

impl Navigator {
    /// Lock, recovering from poisoning: the core has no invariants that a
    /// panic mid-update could leave half-applied in a way that matters more
    /// than staying alive.
    fn lock(&self) -> std::sync::MutexGuard<'_, navcore::Navigator> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl Navigator {
    #[uniffi::constructor]
    pub fn new(route: Route, config: NavigatorConfig) -> Result<Arc<Self>, NavError> {
        let nav = navcore::Navigator::new(route.into(), config.into())?;
        Ok(Arc::new(Self {
            inner: Mutex::new(nav),
        }))
    }

    /// Process one fix. Stale or implausible fixes return an error and leave
    /// the previous state current.
    pub fn update_location(&self, raw: RawLocation) -> Result<FullState, NavError> {
        let update = self.lock().update_location(raw.into())?;
        Ok(FullState {
            position: update.position.into(),
            trip: update.trip.into(),
            trip_changed: update.trip_changed,
        })
    }

    /// Replace the route after the host has rerouted. Answers any pending
    /// reroute request.
    pub fn set_route(&self, route: Route) -> Result<(), NavError> {
        self.lock().set_route(route.into())?;
        Ok(())
    }

    /// Latest per-fix state, if any fix has been accepted since the route was set.
    pub fn position(&self) -> Option<PositionState> {
        self.lock().position().copied().map(PositionState::from)
    }

    /// Latest slow-changing state, if any fix has been accepted since the route was set.
    pub fn trip(&self) -> Option<TripState> {
        self.lock().trip().cloned().map(TripState::from)
    }

    /// Both halves, if any fix has been accepted since the route was set.
    pub fn state(&self) -> Option<FullState> {
        let nav = self.lock();
        let full = nav.full_state()?;
        Some(FullState {
            position: full.position.into(),
            trip: full.trip.into(),
            trip_changed: false,
        })
    }

    /// The reroute request the host has not yet answered with `set_route`.
    pub fn pending_reroute(&self) -> Option<RerouteRequest> {
        self.lock()
            .pending_reroute()
            .copied()
            .map(RerouteRequest::from)
    }

    /// Requests issued since the route was last set.
    pub fn reroute_attempts(&self) -> u32 {
        self.lock().reroute_attempts()
    }

    /// The active route.
    pub fn route(&self) -> Route {
        self.lock().route().clone().into()
    }
}

// ---------------------------------------------------------------------------
// RouteSimulator object
// ---------------------------------------------------------------------------

/// Thread-safe handle over the core route simulator. Same seed, same fixes,
/// on every platform.
#[derive(uniffi::Object)]
pub struct RouteSimulator {
    inner: Mutex<navcore::RouteSimulator>,
}

impl RouteSimulator {
    fn lock(&self) -> std::sync::MutexGuard<'_, navcore::RouteSimulator> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl RouteSimulator {
    #[uniffi::constructor]
    pub fn new(route: Route, config: SimulatorConfig) -> Result<Arc<Self>, NavError> {
        let sim = navcore::RouteSimulator::new(&route.into(), config.into())?;
        Ok(Arc::new(Self {
            inner: Mutex::new(sim),
        }))
    }

    /// Advance by the configured fix interval and emit a fix.
    pub fn next_fix(&self) -> RawLocation {
        self.lock().next_fix().into()
    }

    /// Advance by `dt_s` seconds and emit a fix.
    pub fn advance(&self, dt_s: f64) -> RawLocation {
        self.lock().advance(dt_s).into()
    }

    pub fn set_target_speed(&self, speed_mps: f64) {
        self.lock().set_target_speed(speed_mps);
    }

    pub fn target_speed_mps(&self) -> f64 {
        self.lock().target_speed_mps()
    }

    pub fn set_planned_deviation(&self, deviation: PlannedDeviation) {
        self.lock().set_planned_deviation(deviation.into());
    }

    pub fn clear_planned_deviation(&self) {
        self.lock().clear_planned_deviation();
    }

    /// Multiply position noise and reported accuracy by `sigma_multiplier` (≥ 1).
    pub fn start_signal_degradation(&self, sigma_multiplier: f64) {
        self.lock().start_signal_degradation(sigma_multiplier);
    }

    pub fn stop_signal_degradation(&self) {
        self.lock().stop_signal_degradation();
    }

    pub fn is_signal_degraded(&self) -> bool {
        self.lock().is_signal_degraded()
    }

    /// True once the vehicle has reached the end of the route.
    pub fn is_finished(&self) -> bool {
        self.lock().is_finished()
    }

    pub fn distance_along_m(&self) -> f64 {
        self.lock().distance_along_m()
    }

    pub fn route_length_m(&self) -> f64 {
        self.lock().route_length_m()
    }

    /// Fixes emitted since the last reset.
    pub fn fixes_emitted(&self) -> u64 {
        self.lock().fixes_emitted()
    }

    /// Noise-free position at the last emitted fix.
    pub fn truth(&self) -> GeoPoint {
        self.lock().truth().into()
    }

    /// Rewind to the start with the original seed.
    pub fn reset(&self) {
        self.lock().reset();
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
            speed_mps: s.speed_mps,
            eta_s: s.eta_s,
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

/// Route indices never approach `u32::MAX`; saturate rather than wrap so the
/// conversion can never be silently wrong.
fn index_to_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}
