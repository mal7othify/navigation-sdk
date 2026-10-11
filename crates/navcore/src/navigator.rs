//! The navigator: owns the route, filter and matcher and turns raw fixes into
//! a [`PositionState`] (every fix) and a [`TripState`] (on change), with
//! hysteresis on step advancement, off-route detection and a bounded,
//! rate-limited reroute policy.
//!
//! The per-fix path allocates nothing: the filter and matcher use fixed
//! storage, `PositionState` is `Copy`, and `TripState` shares step
//! instructions through `Arc<str>`.

use std::sync::Arc;

use crate::filter::{FilterConfig, FilteredLocation, LocationFilter};
use crate::geo::{
    bearing_diff_rad, bearing_rad_to_deg, deg_to_rad, Enu, RouteIndex, RouteProjection,
};
use crate::matcher::{HmmMatcher, MatchInput, Matcher, MatcherConfig};
use crate::types::{
    FullState, NavError, PositionState, RawLocation, RerouteReason, RerouteRequest, Route,
    TripProgress, TripState,
};

/// Every tunable in one place. Defaults suit a car at urban speeds with
/// typical phone GNSS (5–15 m accuracy, 1 Hz).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavigatorConfig {
    /// Advance to the next step this close to the current step's end vertex,
    /// provided heading matches the next step. Metres.
    pub step_advance_distance_m: f64,
    /// Heading must be within this of the next step's bearing for the early
    /// advance above. Degrees.
    pub step_advance_heading_tolerance_deg: f64,
    /// Snapped distance from the route beyond which a fix counts as off-route. Metres.
    pub off_route_distance_m: f64,
    /// Consecutive off-route fixes before `is_off_route` becomes true.
    pub off_route_min_consecutive: u32,
    /// Consecutive on-route fixes before `is_off_route` becomes false again.
    pub on_route_min_consecutive: u32,
    /// Off-route fixes only count while the filtered speed is at least this;
    /// a parked vehicle drifting under a bridge never goes off-route. Metres/second.
    pub off_route_min_speed_mps: f64,
    /// The vehicle must have moved at least this far since the off-route
    /// streak began before `is_off_route` becomes true. Metres.
    pub off_route_min_movement_m: f64,
    /// Deviations are ignored for this long after `new` or `set_route`, while
    /// the filter settles and the driver leaves the car park. Seconds.
    pub reroute_grace_period_s: f64,
    /// Deviations are ignored when the vehicle is this close (straight line)
    /// to the destination. Metres.
    pub suppress_off_route_near_destination_m: f64,
    /// Minimum gap between reroute requests. Also the retry interval when the
    /// host has not installed a new route. Seconds.
    pub min_time_between_reroutes_s: f64,
    /// Requests issued without a `set_route` in between before the core gives
    /// up (one final `RerouteReason::GaveUp` is reported). `0` means no limit.
    pub max_reroute_attempts: u32,
    /// Arrived when on the last step and this close to the final vertex. Metres.
    pub arrival_distance_m: f64,
    /// Fixes implying a faster movement than this are rejected. Metres/second.
    pub max_speed_mps: f64,
    /// Kalman process noise (unmodelled acceleration, 1σ). Metres/second².
    pub process_noise_accel_mps2: f64,
    /// Accuracy assumed for fixes that report none. Metres.
    pub default_accuracy_m: f64,
    /// Map-matching candidate search radius. Metres.
    pub candidate_radius_m: f64,
    /// Widened search radius when nothing is found. Metres.
    pub widened_candidate_radius_m: f64,
    /// Backwards movement along the route tolerated before penalties. Metres.
    pub max_backtrack_m: f64,
    /// HMM window length in fixes (clamped to `matcher::MAX_WINDOW`).
    pub hmm_window_size: u32,
}

impl Default for NavigatorConfig {
    fn default() -> Self {
        let f = FilterConfig::default();
        let m = MatcherConfig::default();
        Self {
            step_advance_distance_m: 20.0,
            step_advance_heading_tolerance_deg: 45.0,
            off_route_distance_m: 30.0,
            off_route_min_consecutive: 3,
            on_route_min_consecutive: 2,
            off_route_min_speed_mps: 1.0,
            off_route_min_movement_m: 5.0,
            reroute_grace_period_s: 10.0,
            suppress_off_route_near_destination_m: 50.0,
            min_time_between_reroutes_s: 10.0,
            max_reroute_attempts: 3,
            arrival_distance_m: 15.0,
            max_speed_mps: f.max_speed_mps,
            process_noise_accel_mps2: f.process_noise_accel_mps2,
            default_accuracy_m: f.default_accuracy_m,
            candidate_radius_m: m.candidate_radius_m,
            widened_candidate_radius_m: m.widened_radius_m,
            max_backtrack_m: m.max_backtrack_m,
            hmm_window_size: m.window_size as u32,
        }
    }
}

impl NavigatorConfig {
    fn filter_config(&self) -> FilterConfig {
        FilterConfig {
            process_noise_accel_mps2: self.process_noise_accel_mps2,
            default_accuracy_m: self.default_accuracy_m,
            max_speed_mps: self.max_speed_mps,
            ..FilterConfig::default()
        }
    }

    fn matcher_config(&self) -> MatcherConfig {
        MatcherConfig {
            candidate_radius_m: self.candidate_radius_m,
            widened_radius_m: self.widened_candidate_radius_m,
            window_size: self.hmm_window_size as usize,
            max_backtrack_m: self.max_backtrack_m,
            ..MatcherConfig::default()
        }
    }

    fn validate(&self) -> Result<(), NavError> {
        let positive = [
            ("step_advance_distance_m", self.step_advance_distance_m),
            ("off_route_distance_m", self.off_route_distance_m),
            ("arrival_distance_m", self.arrival_distance_m),
            ("max_speed_mps", self.max_speed_mps),
            ("process_noise_accel_mps2", self.process_noise_accel_mps2),
            ("default_accuracy_m", self.default_accuracy_m),
            ("candidate_radius_m", self.candidate_radius_m),
            (
                "widened_candidate_radius_m",
                self.widened_candidate_radius_m,
            ),
            ("max_backtrack_m", self.max_backtrack_m),
        ];
        for (name, v) in positive {
            if !v.is_finite() || v <= 0.0 {
                return Err(NavError::InvalidConfig(format!("{name} must be positive")));
            }
        }
        let non_negative = [
            (
                "min_time_between_reroutes_s",
                self.min_time_between_reroutes_s,
            ),
            ("off_route_min_speed_mps", self.off_route_min_speed_mps),
            ("off_route_min_movement_m", self.off_route_min_movement_m),
            ("reroute_grace_period_s", self.reroute_grace_period_s),
            (
                "suppress_off_route_near_destination_m",
                self.suppress_off_route_near_destination_m,
            ),
        ];
        for (name, v) in non_negative {
            if !v.is_finite() || v < 0.0 {
                return Err(NavError::InvalidConfig(format!(
                    "{name} must be non-negative"
                )));
            }
        }
        if self.off_route_min_consecutive == 0 || self.on_route_min_consecutive == 0 {
            return Err(NavError::InvalidConfig(
                "consecutive-fix thresholds must be at least 1".into(),
            ));
        }
        Ok(())
    }
}

/// Route-derived data rebuilt on every `set_route`.
struct RouteData {
    route: Route,
    index: RouteIndex,
    /// Along-route distance of each step's end vertex.
    step_end_m: Vec<f64>,
    /// Along-route distance of each step's start vertex.
    step_start_m: Vec<f64>,
    /// Bearing of the route at each step's start, radians.
    step_start_bearing_rad: Vec<f64>,
    /// Shared instruction strings, one per step.
    instructions: Vec<Arc<str>>,
    /// Final vertex in the local frame.
    destination: Enu,
}

impl RouteData {
    fn new(route: Route) -> Result<Self, NavError> {
        route.validate()?;
        let index = RouteIndex::new(&route.geometry)?;
        let last_segment = index.segment_count() - 1;
        let step_end_m = route
            .steps
            .iter()
            .map(|s| index.cumulative_m(s.end_index))
            .collect();
        let step_start_m = route
            .steps
            .iter()
            .map(|s| index.cumulative_m(s.start_index))
            .collect();
        let step_start_bearing_rad = route
            .steps
            .iter()
            .map(|s| index.segment_bearing_rad(s.start_index.min(last_segment)))
            .collect();
        let instructions = route
            .steps
            .iter()
            .map(|s| Arc::<str>::from(s.instruction.as_str()))
            .collect();
        let destination = index.point_at_distance(index.total_length_m());
        Ok(Self {
            route,
            index,
            step_end_m,
            step_start_m,
            step_start_bearing_rad,
            instructions,
            destination,
        })
    }

    fn last_step(&self) -> usize {
        self.route.steps.len() - 1
    }

    /// Step whose extent contains `along_m` (zero-length arrival step only
    /// when at the very end).
    fn step_at(&self, along_m: f64) -> usize {
        let n = self.route.steps.len();
        // Steps with end ≤ along are behind us; the first with end > along is current.
        let i = self.step_end_m.partition_point(|&e| e <= along_m);
        if i >= n {
            n - 1
        } else {
            i
        }
    }
}

/// What one accepted fix produced. Cloning shares the instruction string,
/// so handing it out does not allocate.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// The new per-fix state.
    pub position: PositionState,
    /// The slow-changing state after this fix (also available from
    /// [`Navigator::trip`]).
    pub trip: TripState,
    /// True when `trip` differs from the previous update's trip state or
    /// carries a reroute request. Push-style hosts forward `trip` only when
    /// this is set.
    pub trip_changed: bool,
}

/// Turn-by-turn navigator. See the module docs.
pub struct Navigator {
    config: NavigatorConfig,
    data: RouteData,
    filter: LocationFilter,
    matcher: HmmMatcher,
    position: Option<PositionState>,
    trip: Option<TripState>,
    current_step: usize,
    off_route_streak: u32,
    on_route_streak: u32,
    /// Filtered position where the current off-route streak began.
    off_route_streak_origin: Enu,
    is_off_route: bool,
    arrived: bool,
    last_accepted_ms: Option<u64>,
    /// First accepted fix since `new` / `set_route`; anchors the grace period.
    trip_start_ms: Option<u64>,
    last_reroute_request_ms: Option<u64>,
    pending_reroute: Option<RerouteRequest>,
    reroute_attempts: u32,
    gave_up: bool,
}

impl Navigator {
    /// Build a navigator for `route`. Fails on an invalid route or config.
    pub fn new(route: Route, config: NavigatorConfig) -> Result<Self, NavError> {
        config.validate()?;
        let data = RouteData::new(route)?;
        Ok(Self {
            config,
            filter: LocationFilter::new(config.filter_config()),
            matcher: HmmMatcher::new(config.matcher_config()),
            data,
            position: None,
            trip: None,
            current_step: 0,
            off_route_streak: 0,
            on_route_streak: 0,
            off_route_streak_origin: Enu::new(0.0, 0.0),
            is_off_route: false,
            arrived: false,
            last_accepted_ms: None,
            trip_start_ms: None,
            last_reroute_request_ms: None,
            pending_reroute: None,
            reroute_attempts: 0,
            gave_up: false,
        })
    }

    /// The active route.
    pub fn route(&self) -> &Route {
        &self.data.route
    }

    /// The configuration in use.
    pub fn config(&self) -> &NavigatorConfig {
        &self.config
    }

    /// Spatial index of the active route.
    pub fn route_index(&self) -> &RouteIndex {
        &self.data.index
    }

    /// Latest per-fix state, if any fix has been accepted since the route was set.
    pub fn position(&self) -> Option<&PositionState> {
        self.position.as_ref()
    }

    /// Latest slow-changing state, if any fix has been accepted since the route was set.
    pub fn trip(&self) -> Option<&TripState> {
        self.trip.as_ref()
    }

    /// Both halves together. Clones one `Arc`, no allocation.
    pub fn full_state(&self) -> Option<FullState> {
        Some(FullState {
            position: self.position?,
            trip: self.trip.clone()?,
        })
    }

    /// The reroute request the host has not yet answered with `set_route`,
    /// if any. Cleared when the vehicle rejoins the route on its own.
    pub fn pending_reroute(&self) -> Option<&RerouteRequest> {
        self.pending_reroute.as_ref()
    }

    /// Requests issued since the route was last set.
    pub fn reroute_attempts(&self) -> u32 {
        self.reroute_attempts
    }

    /// Replace the route (after the host rerouted). Trip progress, the
    /// off-route state, the reroute attempt counter and the grace period
    /// restart on the new route; the reroute rate limit and the stale-fix
    /// guard persist.
    pub fn set_route(&mut self, route: Route) -> Result<(), NavError> {
        self.data = RouteData::new(route)?;
        self.filter.reset();
        self.matcher.reset();
        self.position = None;
        self.trip = None;
        self.current_step = 0;
        self.off_route_streak = 0;
        self.on_route_streak = 0;
        self.is_off_route = false;
        self.arrived = false;
        self.trip_start_ms = None;
        self.clear_reroute();
        Ok(())
    }

    /// Process one fix.
    ///
    /// Errors leave the state untouched: stale or implausible fixes are
    /// rejected and the previous [`PositionState`] / [`TripState`] remain
    /// current.
    pub fn update_location(&mut self, raw: RawLocation) -> Result<Update, NavError> {
        if !raw.point.is_valid() {
            return Err(NavError::InvalidLocation);
        }
        if let Some(last) = self.last_accepted_ms {
            if raw.timestamp_ms <= last {
                return Err(NavError::StaleLocation {
                    timestamp_ms: raw.timestamp_ms,
                    last_timestamp_ms: last,
                });
            }
        }

        let enu = self.data.index.frame().to_enu(raw.point);
        let filtered = self.filter.update(enu, &raw)?;
        let now_ms = raw.timestamp_ms;
        self.last_accepted_ms = Some(now_ms);
        if self.trip_start_ms.is_none() {
            self.trip_start_ms = Some(now_ms);
        }

        let input = MatchInput {
            position: filtered.position,
            sigma_m: filtered.position_std_m,
            heading_rad: filtered.heading_rad,
            timestamp_ms: now_ms,
        };
        let proj = self.matcher.match_fix(&self.data.index, &input)?;

        self.update_off_route(&proj, &filtered, now_ms);
        let reroute = self.maybe_request_reroute(&filtered, now_ms);
        if !self.is_off_route {
            self.update_step(&proj, &filtered);
        }

        let (position, trip) = self.build_state(&proj, reroute);
        let trip_changed = reroute.is_some()
            || match &self.trip {
                None => true,
                Some(prev) => !same_trip_ignoring_reroute(prev, &trip),
            };
        self.position = Some(position);
        self.trip = Some(trip.clone());
        Ok(Update {
            position,
            trip,
            trip_changed,
        })
    }

    fn in_grace_period(&self, now_ms: u64) -> bool {
        let grace_ms = (self.config.reroute_grace_period_s * 1000.0) as u64;
        match self.trip_start_ms {
            Some(start) => now_ms.saturating_sub(start) < grace_ms,
            None => true,
        }
    }

    fn update_off_route(
        &mut self,
        proj: &RouteProjection,
        filtered: &FilteredLocation,
        now_ms: u64,
    ) {
        if proj.distance_from_route_m > self.config.off_route_distance_m {
            // Deviations are ignored while settling, when parked, and at the
            // destination (where the last metres are rarely on the polyline).
            let near_destination = filtered.position.distance_to(self.data.destination)
                <= self.config.suppress_off_route_near_destination_m;
            if self.in_grace_period(now_ms)
                || near_destination
                || filtered.speed_mps < self.config.off_route_min_speed_mps
            {
                return;
            }
            if self.off_route_streak == 0 {
                self.off_route_streak_origin = filtered.position;
            }
            self.off_route_streak += 1;
            self.on_route_streak = 0;
            let moved_m = self.off_route_streak_origin.distance_to(filtered.position);
            if self.off_route_streak >= self.config.off_route_min_consecutive
                && moved_m >= self.config.off_route_min_movement_m
            {
                self.is_off_route = true;
            }
        } else {
            self.on_route_streak += 1;
            self.off_route_streak = 0;
            if self.on_route_streak >= self.config.on_route_min_consecutive {
                if self.is_off_route {
                    // Rejoined on our own: forget the outstanding request.
                    self.clear_reroute();
                }
                self.is_off_route = false;
            }
        }
    }

    fn clear_reroute(&mut self) {
        self.pending_reroute = None;
        self.reroute_attempts = 0;
        self.gave_up = false;
    }

    /// Issues a request on the update where one is due: the first off-route
    /// fix, then every `min_time_between_reroutes_s` while the host has not
    /// installed a route, up to `max_reroute_attempts`, then one `GaveUp`.
    fn maybe_request_reroute(
        &mut self,
        filtered: &FilteredLocation,
        now_ms: u64,
    ) -> Option<RerouteRequest> {
        if !self.is_off_route || self.arrived || self.gave_up {
            return None;
        }
        let min_gap_ms = (self.config.min_time_between_reroutes_s * 1000.0) as u64;
        let due = match self.last_reroute_request_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= min_gap_ms,
        };
        if !due {
            return None;
        }
        let limit = self.config.max_reroute_attempts;
        let frame = self.data.index.frame();
        let mut request = RerouteRequest {
            origin: frame.to_geo(filtered.position),
            heading_deg: filtered.heading_rad.map(bearing_rad_to_deg),
            destination: frame.to_geo(self.data.destination),
            attempt: self.reroute_attempts,
            reason: RerouteReason::GaveUp,
            timestamp_ms: now_ms,
        };
        if limit != 0 && self.reroute_attempts >= limit {
            self.gave_up = true;
            self.pending_reroute = None;
            return Some(request);
        }
        self.reroute_attempts += 1;
        self.last_reroute_request_ms = Some(now_ms);
        request.attempt = self.reroute_attempts;
        request.reason = RerouteReason::OffRoute;
        self.pending_reroute = Some(request);
        Some(request)
    }

    fn update_step(&mut self, proj: &RouteProjection, filtered: &FilteredLocation) {
        if self.arrived {
            return;
        }
        let along = proj.distance_along_route_m;
        let last = self.data.last_step();

        // Large backwards jump (matcher recovered from a wrong match): resync.
        if along < self.data.step_start_m[self.current_step] - self.config.max_backtrack_m {
            self.current_step = self.data.step_at(along);
        }

        // Passed the end vertex of the current step (possibly several).
        while self.current_step < last && along >= self.data.step_end_m[self.current_step] {
            self.current_step += 1;
        }

        // Early advance: close to the end vertex and already heading like the next step.
        if self.current_step < last {
            let remaining = self.data.step_end_m[self.current_step] - along;
            if remaining <= self.config.step_advance_distance_m {
                if let Some(heading) = filtered.heading_rad {
                    let next_bearing = self.data.step_start_bearing_rad[self.current_step + 1];
                    let tol = deg_to_rad(self.config.step_advance_heading_tolerance_deg);
                    if bearing_diff_rad(heading, next_bearing).abs() <= tol {
                        self.current_step += 1;
                    }
                }
            }
        }

        // Arrival: on the last driving or arrival step, near the final vertex.
        let total = self.data.index.total_length_m();
        if self.current_step >= last.saturating_sub(1)
            && total - along <= self.config.arrival_distance_m
        {
            self.current_step = last;
            self.arrived = true;
        }
    }

    fn build_state(
        &self,
        proj: &RouteProjection,
        reroute: Option<RerouteRequest>,
    ) -> (PositionState, TripState) {
        let along = proj.distance_along_route_m;
        let total = self.data.index.total_length_m();
        let last = self.data.last_step();
        let step = self.current_step;
        let next = (step + 1).min(last);
        let progress = if self.arrived {
            TripProgress::Arrived
        } else {
            TripProgress::Navigating
        };
        let position = PositionState {
            snapped: self.data.index.to_snapped(proj),
            distance_to_next_maneuver_m: (self.data.step_end_m[step] - along).max(0.0),
            distance_remaining_m: (total - along).max(0.0),
            is_off_route: self.is_off_route,
            progress,
        };
        let trip = TripState {
            current_step_index: step,
            step_count: self.data.route.steps.len(),
            next_instruction: Arc::clone(&self.data.instructions[next]),
            is_off_route: self.is_off_route,
            progress,
            reroute,
        };
        (position, trip)
    }
}

/// Equality on everything but the per-update `reroute` edge.
fn same_trip_ignoring_reroute(a: &TripState, b: &TripState) -> bool {
    a.current_step_index == b.current_step_index
        && a.step_count == b.step_count
        && Arc::ptr_eq(&a.next_instruction, &b.next_instruction)
        && a.is_off_route == b.is_off_route
        && a.progress == b.progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{Enu, LocalFrame};
    use crate::types::{GeoPoint, ManeuverType, RouteStep};

    /// East 300 m, then north 300 m; 50 m vertex spacing.
    fn l_route() -> (Route, LocalFrame) {
        let frame = LocalFrame::new(GeoPoint::new(45.0, 7.0));
        let mut pts = Vec::new();
        for i in 0..=6 {
            pts.push(Enu::new(i as f64 * 50.0, 0.0));
        }
        for j in 1..=6 {
            pts.push(Enu::new(300.0, j as f64 * 50.0));
        }
        let geometry: Vec<GeoPoint> = pts.iter().map(|e| frame.to_geo(*e)).collect();
        let step = |i: &str, m, s, e, d| RouteStep {
            instruction: i.to_string(),
            maneuver: m,
            start_index: s,
            end_index: e,
            distance_m: d,
        };
        let route = Route {
            geometry,
            steps: vec![
                step("Head east", ManeuverType::Depart, 0, 6, 300.0),
                step("Turn left", ManeuverType::TurnLeft, 6, 12, 300.0),
                step("Arrive", ManeuverType::Arrive, 12, 12, 0.0),
            ],
        };
        (route, frame)
    }

    fn fix(frame: &LocalFrame, x: f64, y: f64, t_s: u64, course: Option<f64>) -> RawLocation {
        RawLocation {
            point: frame.to_geo(Enu::new(x, y)),
            accuracy_m: Some(5.0),
            speed_mps: course.map(|_| 10.0),
            course_deg: course,
            timestamp_ms: 1_000_000 + t_s * 1000,
        }
    }

    /// Update and return the combined state (tests assert on both halves).
    fn step(nav: &mut Navigator, raw: RawLocation) -> Result<(FullState, bool), NavError> {
        let u = nav.update_location(raw)?;
        Ok((
            nav.full_state().expect("state after update"),
            u.trip_changed,
        ))
    }

    #[test]
    fn rejects_bad_route_and_config() {
        let (route, _) = l_route();
        let bad_cfg = NavigatorConfig {
            off_route_distance_m: -1.0,
            ..NavigatorConfig::default()
        };
        assert!(matches!(
            Navigator::new(route.clone(), bad_cfg),
            Err(NavError::InvalidConfig(_))
        ));
        let bad_cfg = NavigatorConfig {
            reroute_grace_period_s: -1.0,
            ..NavigatorConfig::default()
        };
        assert!(matches!(
            Navigator::new(route.clone(), bad_cfg),
            Err(NavError::InvalidConfig(_))
        ));
        let mut bad_route = route;
        bad_route.steps.pop();
        assert!(matches!(
            Navigator::new(bad_route, NavigatorConfig::default()),
            Err(NavError::InvalidRoute(_))
        ));
    }

    #[test]
    fn drives_through_steps_and_arrives() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        assert!(nav.position().is_none());
        assert!(nav.trip().is_none());
        assert!(nav.full_state().is_none());

        let mut seen = Vec::new();
        let mut trip_changes = 0;
        let mut t = 0;
        // East leg at 10 m/s.
        for k in 0..=30 {
            let (s, changed) =
                step(&mut nav, fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0))).unwrap();
            t += 1;
            trip_changes += changed as usize;
            if seen.last() != Some(&s.trip.current_step_index) {
                seen.push(s.trip.current_step_index);
            }
            assert!(!s.position.is_off_route);
            assert!(!s.needs_reroute());
            assert_eq!(s.trip.step_count, 3);
        }
        // North leg.
        let mut final_state = None;
        for k in 1..=30 {
            let (s, changed) =
                step(&mut nav, fix(&frame, 300.0, k as f64 * 10.0, t, Some(0.0))).unwrap();
            t += 1;
            trip_changes += changed as usize;
            if seen.last() != Some(&s.trip.current_step_index) {
                seen.push(s.trip.current_step_index);
            }
            final_state = Some(s);
        }
        assert_eq!(seen, vec![0, 1, 2]);
        // First fix, two step advances (arrival coincides with the last).
        assert_eq!(trip_changes, 3);
        let s = final_state.unwrap();
        assert_eq!(s.position.progress, TripProgress::Arrived);
        assert_eq!(s.trip.progress, TripProgress::Arrived);
        assert!(s.position.distance_remaining_m < 15.0);
        assert_eq!(&*s.trip.next_instruction, "Arrive");
        // Arrival is sticky.
        let (s, changed) = step(&mut nav, fix(&frame, 300.0, 310.0, t, Some(0.0))).unwrap();
        assert_eq!(s.trip.progress, TripProgress::Arrived);
        assert!(!changed);
    }

    #[test]
    fn next_instruction_and_distance_to_maneuver() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let (s, changed) = step(&mut nav, fix(&frame, 100.0, 0.0, 0, Some(90.0))).unwrap();
        assert!(changed, "first update always reports a trip change");
        assert_eq!(s.trip.current_step_index, 0);
        assert_eq!(&*s.trip.next_instruction, "Turn left");
        assert!((s.position.distance_to_next_maneuver_m - 200.0).abs() < 1.0);
        assert!((s.position.distance_remaining_m - 500.0).abs() < 1.0);
        assert_eq!(s.position.progress, TripProgress::Navigating);
    }

    #[test]
    fn early_advance_requires_heading_match() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let mut t = 0;
        for k in 0..=28 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        // 15 m short of the corner, still heading east → no advance.
        let (s, _) = step(&mut nav, fix(&frame, 285.0, 0.0, t, Some(90.0))).unwrap();
        t += 1;
        assert_eq!(s.trip.current_step_index, 0);
        // Same distance but heading north: the filter needs a couple of fixes
        // to swing its heading estimate, then we advance.
        let mut advanced = false;
        for k in 0..4 {
            let (s, changed) = step(
                &mut nav,
                fix(&frame, 288.0, 3.0 + k as f64 * 3.0, t, Some(0.0)),
            )
            .unwrap();
            t += 1;
            if s.trip.current_step_index == 1 {
                assert!(changed);
                advanced = true;
                break;
            }
        }
        assert!(advanced);
    }

    #[test]
    fn off_route_needs_consecutive_fixes_and_reroute_is_rate_limited() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            min_time_between_reroutes_s: 5.0,
            max_reroute_attempts: 0,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route, cfg).unwrap();
        let mut t = 0;
        for k in 0..=10 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        // Veer 80 m south of the route and keep going east.
        let mut reroutes = Vec::new();
        let mut first_off = None;
        for k in 0..12 {
            let (s, changed) = step(
                &mut nav,
                fix(&frame, 110.0 + k as f64 * 10.0, -80.0, t, Some(90.0)),
            )
            .unwrap();
            if s.position.is_off_route && first_off.is_none() {
                first_off = Some(k);
                assert!(changed, "off-route transition is a trip change");
            }
            if let Some(req) = s.trip.reroute {
                assert!(changed, "a reroute request is a trip change");
                assert_eq!(req.reason, RerouteReason::OffRoute);
                assert_eq!(req.timestamp_ms, 1_000_000 + t * 1000);
                assert_eq!(nav.pending_reroute(), Some(&req));
                reroutes.push((t, req.attempt));
            }
            t += 1;
        }
        // Off-route flagged only after ≥3 consecutive far fixes (the filter
        // lags, so it may take a fix or two more than that).
        let first_off = first_off.expect("should go off-route");
        assert!(
            (2..=5).contains(&first_off),
            "first off-route at {first_off}"
        );
        assert!(!reroutes.is_empty());
        for (i, (_, attempt)) in reroutes.iter().enumerate() {
            assert_eq!(*attempt as usize, i + 1);
        }
        for w in reroutes.windows(2) {
            assert!(w[1].0 - w[0].0 >= 5, "reroutes too close: {reroutes:?}");
        }
        // Come back onto the route: two good fixes clear the flag and drop
        // the pending request.
        let mut cleared_after = None;
        for k in 0..6 {
            let (s, _) = step(
                &mut nav,
                fix(&frame, 240.0 + k as f64 * 5.0, 0.0, t, Some(90.0)),
            )
            .unwrap();
            t += 1;
            if !s.position.is_off_route {
                cleared_after = Some(k);
                break;
            }
        }
        // The 80 m step back onto the route is a teleport as far as the
        // filter is concerned, so allow a few fixes of lag before the two
        // consecutive good fixes clear the flag.
        assert!(cleared_after.is_some_and(|k| k <= 6), "{cleared_after:?}");
        assert!(nav.pending_reroute().is_none());
        assert_eq!(nav.reroute_attempts(), 0);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            min_time_between_reroutes_s: 2.0,
            max_reroute_attempts: 2,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route, cfg).unwrap();
        let mut t = 0;
        for k in 0..=10 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        let mut events = Vec::new();
        for k in 0..16 {
            let (s, _) = step(
                &mut nav,
                fix(&frame, 110.0 + k as f64 * 10.0, -80.0, t, Some(90.0)),
            )
            .unwrap();
            if let Some(req) = s.trip.reroute {
                events.push((req.reason, req.attempt));
            }
            t += 1;
        }
        assert_eq!(
            events,
            vec![
                (RerouteReason::OffRoute, 1),
                (RerouteReason::OffRoute, 2),
                (RerouteReason::GaveUp, 2),
            ]
        );
        assert!(nav.pending_reroute().is_none());
        assert_eq!(nav.reroute_attempts(), 2);
    }

    #[test]
    fn grace_period_suppresses_early_deviations() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            reroute_grace_period_s: 30.0,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route, cfg).unwrap();
        // Start 80 m south of the route and drive east: off the route from
        // the first fix, but inside the grace period for 30 s.
        let mut first_off = None;
        for t in 0..40u64 {
            let (s, _) = step(&mut nav, fix(&frame, t as f64 * 5.0, -80.0, t, Some(90.0))).unwrap();
            if s.position.is_off_route && first_off.is_none() {
                first_off = Some(t);
            }
        }
        let first_off = first_off.expect("eventually off-route");
        assert!(first_off >= 30 + 2, "flagged at {first_off} s");
        assert!(first_off <= 30 + 6, "flagged at {first_off} s");
    }

    #[test]
    fn parked_vehicle_never_goes_off_route() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            reroute_grace_period_s: 0.0,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route, cfg).unwrap();
        // Stationary 60 m off the route with zero reported speed.
        for t in 0..30u64 {
            let mut raw = fix(&frame, 150.0, -60.0, t, None);
            raw.speed_mps = Some(0.0);
            let (s, _) = step(&mut nav, raw).unwrap();
            assert!(!s.position.is_off_route, "fix {t}");
            assert!(s.trip.reroute.is_none());
        }
    }

    #[test]
    fn deviations_near_destination_are_suppressed() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            reroute_grace_period_s: 0.0,
            suppress_off_route_near_destination_m: 120.0,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route, cfg).unwrap();
        // Drive the whole route, then wander 60 m east of the final vertex
        // (300, 300) at speed: within 120 m of it, so never off-route.
        let mut t = 0;
        for k in 0..=30 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        for k in 1..=28 {
            nav.update_location(fix(&frame, 300.0, k as f64 * 10.0, t, Some(0.0)))
                .unwrap();
            t += 1;
        }
        for k in 0..10 {
            let (s, _) = step(
                &mut nav,
                fix(&frame, 360.0, 250.0 + k as f64 * 5.0, t, Some(0.0)),
            )
            .unwrap();
            t += 1;
            assert!(!s.position.is_off_route, "fix {k}");
        }
    }

    #[test]
    fn stale_and_implausible_fixes_keep_previous_state() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let (s0, _) = step(&mut nav, fix(&frame, 50.0, 0.0, 10, Some(90.0))).unwrap();
        assert!(matches!(
            nav.update_location(fix(&frame, 60.0, 0.0, 10, Some(90.0))),
            Err(NavError::StaleLocation { .. })
        ));
        assert!(matches!(
            nav.update_location(fix(&frame, 5000.0, 0.0, 11, Some(90.0))),
            Err(NavError::ImplausibleLocation { .. })
        ));
        let mut bad = fix(&frame, 60.0, 0.0, 12, None);
        bad.point.lat = 100.0;
        assert_eq!(nav.update_location(bad), Err(NavError::InvalidLocation));
        assert_eq!(nav.full_state(), Some(s0));
    }

    #[test]
    fn set_route_restarts_progress_but_keeps_time_guard() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route.clone(), NavigatorConfig::default()).unwrap();
        for k in 0..=35 {
            nav.update_location(fix(
                &frame,
                (k as f64 * 10.0).min(300.0),
                (k as f64 * 10.0 - 300.0).max(0.0),
                k,
                None,
            ))
            .unwrap();
        }
        assert_eq!(nav.trip().unwrap().current_step_index, 1);
        nav.set_route(route).unwrap();
        assert!(nav.position().is_none());
        assert!(nav.trip().is_none());
        // Older fix than the last accepted one is still rejected.
        assert!(matches!(
            nav.update_location(fix(&frame, 0.0, 0.0, 3, None)),
            Err(NavError::StaleLocation { .. })
        ));
        let (s, changed) = step(&mut nav, fix(&frame, 20.0, 0.0, 40, Some(90.0))).unwrap();
        assert!(changed);
        assert_eq!(s.trip.current_step_index, 0);
        assert_eq!(s.trip.progress, TripProgress::Navigating);
    }

    #[test]
    fn set_route_answers_pending_reroute() {
        let (route, frame) = l_route();
        let cfg = NavigatorConfig {
            min_time_between_reroutes_s: 5.0,
            ..NavigatorConfig::default()
        };
        let mut nav = Navigator::new(route.clone(), cfg).unwrap();
        let mut t = 0;
        for k in 0..=10 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        let mut requested = false;
        for k in 0..8 {
            nav.update_location(fix(&frame, 110.0 + k as f64 * 10.0, -80.0, t, Some(90.0)))
                .unwrap();
            t += 1;
            if nav.pending_reroute().is_some() {
                requested = true;
                break;
            }
        }
        assert!(requested);
        assert_eq!(nav.reroute_attempts(), 1);
        nav.set_route(route).unwrap();
        assert!(nav.pending_reroute().is_none());
        assert_eq!(nav.reroute_attempts(), 0);
    }

    #[test]
    fn step_at_maps_distances() {
        let (route, _) = l_route();
        let data = RouteData::new(route).unwrap();
        assert_eq!(data.step_at(0.0), 0);
        assert_eq!(data.step_at(150.0), 0);
        assert_eq!(data.step_at(300.0), 1);
        assert_eq!(data.step_at(450.0), 1);
        assert_eq!(data.step_at(600.0), 2);
        assert_eq!(data.step_at(1000.0), 2);
    }
}
