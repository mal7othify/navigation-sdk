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

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TripState {
    pub snapped: SnappedLocation,
    pub current_step_index: u32,
    pub distance_to_next_maneuver_m: f64,
    pub distance_remaining_m: f64,
    pub next_instruction: String,
    pub is_off_route: bool,
    /// True only on the update that issues a (rate-limited) reroute request.
    pub needs_reroute: bool,
    pub progress: TripProgress,
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
    #[uniffi(default = 10.0)]
    pub min_time_between_reroutes_s: f64,
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
    pub fn update_location(&self, raw: RawLocation) -> Result<TripState, NavError> {
        let state = self.lock().update_location(raw.into())?;
        Ok(state.into())
    }

    /// Replace the route after the host has rerouted.
    pub fn set_route(&self, route: Route) -> Result<(), NavError> {
        self.lock().set_route(route.into())?;
        Ok(())
    }

    /// Latest state, if any fix has been accepted since the route was set.
    pub fn state(&self) -> Option<TripState> {
        self.lock().state().cloned().map(TripState::from)
    }

    /// The active route.
    pub fn route(&self) -> Route {
        self.lock().route().clone().into()
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

impl From<navcore::TripState> for TripState {
    fn from(s: navcore::TripState) -> Self {
        TripState {
            snapped: s.snapped.into(),
            current_step_index: index_to_u32(s.current_step_index),
            distance_to_next_maneuver_m: s.distance_to_next_maneuver_m,
            distance_remaining_m: s.distance_remaining_m,
            next_instruction: s.next_instruction.to_string(),
            is_off_route: s.is_off_route,
            needs_reroute: s.needs_reroute,
            progress: s.progress.into(),
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
            min_time_between_reroutes_s: c.min_time_between_reroutes_s,
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
            min_time_between_reroutes_s: c.min_time_between_reroutes_s,
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
