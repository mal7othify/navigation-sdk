//! The navigator: owns the route, filter and matcher and turns raw fixes into
//! [`TripState`]s with hysteresis on step advancement, off-route detection
//! and rate-limited reroute requests.
//!
//! The per-fix path allocates nothing: the filter and matcher use fixed
//! storage, and `TripState` shares step instructions through `Arc<str>`.

use std::sync::Arc;

use crate::filter::{FilterConfig, FilteredLocation, LocationFilter};
use crate::geo::{bearing_diff_rad, deg_to_rad, RouteIndex, RouteProjection};
use crate::matcher::{HmmMatcher, MatchInput, Matcher, MatcherConfig};
use crate::types::{NavError, RawLocation, Route, TripProgress, TripState};

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
    /// Minimum gap between reroute requests. Seconds.
    pub min_time_between_reroutes_s: f64,
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
            min_time_between_reroutes_s: 10.0,
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
        if !self.min_time_between_reroutes_s.is_finite() || self.min_time_between_reroutes_s < 0.0 {
            return Err(NavError::InvalidConfig(
                "min_time_between_reroutes_s must be non-negative".into(),
            ));
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
        Ok(Self {
            route,
            index,
            step_end_m,
            step_start_m,
            step_start_bearing_rad,
            instructions,
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

/// Turn-by-turn navigator. See the module docs.
pub struct Navigator {
    config: NavigatorConfig,
    data: RouteData,
    filter: LocationFilter,
    matcher: HmmMatcher,
    state: Option<TripState>,
    current_step: usize,
    off_route_streak: u32,
    on_route_streak: u32,
    is_off_route: bool,
    arrived: bool,
    last_accepted_ms: Option<u64>,
    last_reroute_request_ms: Option<u64>,
    /// Exponential moving average of filtered speed while moving, for the ETA.
    avg_speed_mps: f64,
    /// Seconds of movement folded into `avg_speed_mps` (saturates at `ETA_WARMUP_S`).
    avg_speed_warmup_s: f64,
}

/// Time constant of the moving-average speed behind `TripState::eta_s`.
const ETA_SPEED_TAU_S: f64 = 30.0;
/// Movement needed before an ETA is reported at all.
const ETA_WARMUP_S: f64 = 10.0;
/// Below this filtered speed the trip counts as stopped: the average is held
/// and the ETA keeps its last value instead of shooting to infinity.
const ETA_MIN_MOVING_MPS: f64 = 0.5;

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
            state: None,
            current_step: 0,
            off_route_streak: 0,
            on_route_streak: 0,
            is_off_route: false,
            arrived: false,
            last_accepted_ms: None,
            last_reroute_request_ms: None,
            avg_speed_mps: 0.0,
            avg_speed_warmup_s: 0.0,
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

    /// Latest state, if any fix has been accepted since the route was set.
    pub fn state(&self) -> Option<&TripState> {
        self.state.as_ref()
    }

    /// Replace the route (after the host rerouted). Trip progress restarts on
    /// the new route; the reroute rate limit and the stale-fix guard persist.
    pub fn set_route(&mut self, route: Route) -> Result<(), NavError> {
        self.data = RouteData::new(route)?;
        self.filter.reset();
        self.matcher.reset();
        self.state = None;
        self.current_step = 0;
        self.off_route_streak = 0;
        self.on_route_streak = 0;
        self.is_off_route = false;
        self.arrived = false;
        Ok(())
    }

    /// Process one fix and return the new state.
    ///
    /// Errors leave the state untouched: stale or implausible fixes are
    /// rejected and the previous [`TripState`] remains current.
    pub fn update_location(&mut self, raw: RawLocation) -> Result<TripState, NavError> {
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
        let dt_s = self
            .last_accepted_ms
            .map(|last| raw.timestamp_ms.saturating_sub(last) as f64 / 1000.0)
            .unwrap_or(0.0);
        self.last_accepted_ms = Some(raw.timestamp_ms);
        self.update_avg_speed(filtered.speed_mps, dt_s);

        let input = MatchInput {
            position: filtered.position,
            sigma_m: filtered.position_std_m,
            heading_rad: filtered.heading_rad,
            timestamp_ms: raw.timestamp_ms,
        };
        let proj = self.matcher.match_fix(&self.data.index, &input)?;

        self.update_off_route(&proj);
        let needs_reroute = self.maybe_request_reroute(raw.timestamp_ms);
        // Steps and arrival only move on fixes that are actually near the
        // route. Without the distance check a fix far from the whole route
        // (the matcher falls back to the nearest segment, often the final
        // vertex) would advance through every step and "arrive" on the first
        // update, before the off-route hysteresis has had its three fixes.
        if !self.is_off_route && proj.distance_from_route_m <= self.config.off_route_distance_m {
            self.update_step(&proj, &filtered);
        }

        let state = self.build_state(&proj, &filtered, needs_reroute);
        self.state = Some(state.clone());
        Ok(state)
    }

    /// Fold one filtered speed sample into the moving average. Stopped
    /// samples hold the average so a red light does not inflate the ETA.
    fn update_avg_speed(&mut self, speed_mps: f64, dt_s: f64) {
        if speed_mps < ETA_MIN_MOVING_MPS || dt_s <= 0.0 {
            return;
        }
        if self.avg_speed_warmup_s == 0.0 {
            self.avg_speed_mps = speed_mps;
        } else {
            let alpha = 1.0 - (-dt_s / ETA_SPEED_TAU_S).exp();
            self.avg_speed_mps += alpha * (speed_mps - self.avg_speed_mps);
        }
        self.avg_speed_warmup_s = (self.avg_speed_warmup_s + dt_s).min(ETA_WARMUP_S);
    }

    fn update_off_route(&mut self, proj: &RouteProjection) {
        if proj.distance_from_route_m > self.config.off_route_distance_m {
            self.off_route_streak += 1;
            self.on_route_streak = 0;
            if self.off_route_streak >= self.config.off_route_min_consecutive {
                self.is_off_route = true;
            }
        } else {
            self.on_route_streak += 1;
            self.off_route_streak = 0;
            if self.on_route_streak >= self.config.on_route_min_consecutive {
                self.is_off_route = false;
            }
        }
    }

    /// True on the update where a reroute request is issued.
    fn maybe_request_reroute(&mut self, now_ms: u64) -> bool {
        if !self.is_off_route || self.arrived {
            return false;
        }
        let min_gap_ms = (self.config.min_time_between_reroutes_s * 1000.0) as u64;
        let due = match self.last_reroute_request_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= min_gap_ms,
        };
        if due {
            self.last_reroute_request_ms = Some(now_ms);
        }
        due
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
        filtered: &FilteredLocation,
        needs_reroute: bool,
    ) -> TripState {
        let along = proj.distance_along_route_m;
        let total = self.data.index.total_length_m();
        let last = self.data.last_step();
        let step = self.current_step;
        let next = (step + 1).min(last);
        let remaining = (total - along).max(0.0);
        let eta_s = if self.arrived {
            Some(0.0)
        } else if self.avg_speed_warmup_s >= ETA_WARMUP_S {
            Some(remaining / self.avg_speed_mps)
        } else {
            None
        };
        TripState {
            snapped: self.data.index.to_snapped(proj),
            current_step_index: step,
            distance_to_next_maneuver_m: (self.data.step_end_m[step] - along).max(0.0),
            distance_remaining_m: remaining,
            next_instruction: Arc::clone(&self.data.instructions[next]),
            is_off_route: self.is_off_route,
            needs_reroute,
            speed_mps: filtered.speed_mps,
            eta_s,
            progress: if self.arrived {
                TripProgress::Arrived
            } else {
                TripProgress::Navigating
            },
        }
    }
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
        assert!(nav.state().is_none());

        let mut seen = Vec::new();
        let mut t = 0;
        // East leg at 10 m/s.
        for k in 0..=30 {
            let s = nav
                .update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
            if seen.last() != Some(&s.current_step_index) {
                seen.push(s.current_step_index);
            }
            assert!(!s.is_off_route);
            assert!(!s.needs_reroute);
        }
        // North leg.
        let mut final_state = None;
        for k in 1..=30 {
            let s = nav
                .update_location(fix(&frame, 300.0, k as f64 * 10.0, t, Some(0.0)))
                .unwrap();
            t += 1;
            if seen.last() != Some(&s.current_step_index) {
                seen.push(s.current_step_index);
            }
            final_state = Some(s);
        }
        assert_eq!(seen, vec![0, 1, 2]);
        let s = final_state.unwrap();
        assert_eq!(s.progress, TripProgress::Arrived);
        assert!(s.distance_remaining_m < 15.0);
        assert_eq!(&*s.next_instruction, "Arrive");
        // Arrival is sticky.
        let s = nav
            .update_location(fix(&frame, 300.0, 310.0, t, Some(0.0)))
            .unwrap();
        assert_eq!(s.progress, TripProgress::Arrived);
    }

    #[test]
    fn next_instruction_and_distance_to_maneuver() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let s = nav
            .update_location(fix(&frame, 100.0, 0.0, 0, Some(90.0)))
            .unwrap();
        assert_eq!(s.current_step_index, 0);
        assert_eq!(&*s.next_instruction, "Turn left");
        assert!((s.distance_to_next_maneuver_m - 200.0).abs() < 1.0);
        assert!((s.distance_remaining_m - 500.0).abs() < 1.0);
        assert_eq!(s.progress, TripProgress::Navigating);
    }

    #[test]
    fn fixes_far_from_the_route_never_advance_or_arrive() {
        // Regression: standing kilometres from the route, the nearest segment
        // is the final one, so along-route distance alone said "arrived" on
        // the very first fix.
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let mut off_route_seen = false;
        for k in 0..10 {
            let s = nav
                .update_location(fix(&frame, 5000.0 + k as f64 * 1.5, 5000.0, k, Some(90.0)))
                .unwrap();
            assert_eq!(s.current_step_index, 0, "fix {k} advanced");
            assert_eq!(s.progress, TripProgress::Navigating, "fix {k} arrived");
            assert_eq!(&*s.next_instruction, "Turn left");
            assert!(s.snapped.distance_from_route_m > 1000.0);
            off_route_seen |= s.is_off_route;
        }
        assert!(off_route_seen, "should be flagged off-route instead");
    }

    /// A loop whose destination is its start (a walking course, a delivery
    /// round) must not "arrive" on the first fix just because GPS noise put
    /// that fix a metre or two on the final leg's side of the origin.
    #[test]
    fn closed_loop_does_not_arrive_on_the_first_fix() {
        let frame = LocalFrame::new(GeoPoint::new(45.0, 7.0));
        // 30 m square, counter-clockwise: north, west, south, east back home.
        let corners = [
            (0.0, 0.0),
            (0.0, 30.0),
            (-30.0, 30.0),
            (-30.0, 0.0),
            (0.0, 0.0),
        ];
        let mut pts = Vec::new();
        for w in corners.windows(2) {
            let (ax, ay) = w[0];
            let (bx, by) = w[1];
            for i in 0..3 {
                let t = i as f64 / 3.0;
                pts.push(Enu::new(ax + (bx - ax) * t, ay + (by - ay) * t));
            }
        }
        pts.push(Enu::new(0.0, 0.0));
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
                step("North", ManeuverType::Depart, 0, 3, 30.0),
                step("West", ManeuverType::TurnLeft, 3, 6, 30.0),
                step("South", ManeuverType::TurnLeft, 6, 9, 30.0),
                step("East", ManeuverType::TurnLeft, 9, 12, 30.0),
                step("Arrive", ManeuverType::Arrive, 12, 12, 0.0),
            ],
        };
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        // Standing still at the start, 2 m east: nearer the final (eastbound)
        // leg's end than the first leg, and no heading yet.
        for k in 0..5 {
            let s = nav
                .update_location(RawLocation {
                    point: frame.to_geo(Enu::new(2.0, -1.0)),
                    accuracy_m: Some(8.0),
                    speed_mps: None,
                    course_deg: None,
                    timestamp_ms: 1_000_000 + k * 1000,
                })
                .unwrap();
            assert_eq!(s.current_step_index, 0, "fix {k}");
            assert_eq!(s.progress, TripProgress::Navigating, "fix {k}");
            assert!(
                s.distance_remaining_m > 100.0,
                "fix {k}: {}",
                s.distance_remaining_m
            );
        }
    }

    #[test]
    fn speed_and_eta_follow_the_filtered_velocity() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        // First fix: nothing to average yet.
        let s = nav
            .update_location(fix(&frame, 0.0, 0.0, 0, Some(90.0)))
            .unwrap();
        assert_eq!(s.eta_s, None);
        // Cruise east at 10 m/s for 20 s; ETA should settle near remaining / 10.
        let mut last = s;
        for k in 1..=20 {
            last = nav
                .update_location(fix(&frame, k as f64 * 10.0, 0.0, k, Some(90.0)))
                .unwrap();
        }
        assert!(
            (last.speed_mps - 10.0).abs() < 1.5,
            "speed {}",
            last.speed_mps
        );
        let eta = last.eta_s.expect("eta after warm-up");
        let expected = last.distance_remaining_m / 10.0;
        assert!(
            (eta - expected).abs() < expected * 0.2,
            "eta {eta} vs {expected}"
        );
        // Finish the east leg, turn north, arrive: the ETA pins to zero.
        let mut t = 21;
        for k in 21..=30 {
            nav.update_location(fix(&frame, k as f64 * 10.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
        }
        for k in 1..=30 {
            last = nav
                .update_location(fix(&frame, 300.0, k as f64 * 10.0, t, Some(0.0)))
                .unwrap();
            t += 1;
        }
        assert_eq!(last.progress, TripProgress::Arrived);
        assert_eq!(last.eta_s, Some(0.0));
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
        let s = nav
            .update_location(fix(&frame, 285.0, 0.0, t, Some(90.0)))
            .unwrap();
        t += 1;
        assert_eq!(s.current_step_index, 0);
        // Same distance but heading north: the filter needs a couple of fixes
        // to swing its heading estimate, then we advance.
        let mut advanced = false;
        for k in 0..4 {
            let s = nav
                .update_location(fix(&frame, 288.0, 3.0 + k as f64 * 3.0, t, Some(0.0)))
                .unwrap();
            t += 1;
            if s.current_step_index == 1 {
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
            let s = nav
                .update_location(fix(&frame, 110.0 + k as f64 * 10.0, -80.0, t, Some(90.0)))
                .unwrap();
            if s.is_off_route && first_off.is_none() {
                first_off = Some(k);
            }
            if s.needs_reroute {
                reroutes.push(t);
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
        for w in reroutes.windows(2) {
            assert!(w[1] - w[0] >= 5, "reroutes too close: {reroutes:?}");
        }
        // Come back onto the route: two good fixes clear the flag.
        let mut cleared_after = None;
        for k in 0..6 {
            let s = nav
                .update_location(fix(&frame, 240.0 + k as f64 * 5.0, 0.0, t, Some(90.0)))
                .unwrap();
            t += 1;
            if !s.is_off_route {
                cleared_after = Some(k);
                break;
            }
        }
        // The 80 m step back onto the route is a teleport as far as the
        // filter is concerned, so allow a few fixes of lag before the two
        // consecutive good fixes clear the flag.
        assert!(cleared_after.is_some_and(|k| k <= 6), "{cleared_after:?}");
    }

    #[test]
    fn stale_and_implausible_fixes_keep_previous_state() {
        let (route, frame) = l_route();
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();
        let s0 = nav
            .update_location(fix(&frame, 50.0, 0.0, 10, Some(90.0)))
            .unwrap();
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
        assert_eq!(nav.state(), Some(&s0));
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
        assert_eq!(nav.state().unwrap().current_step_index, 1);
        nav.set_route(route).unwrap();
        assert!(nav.state().is_none());
        // Older fix than the last accepted one is still rejected.
        assert!(matches!(
            nav.update_location(fix(&frame, 0.0, 0.0, 3, None)),
            Err(NavError::StaleLocation { .. })
        ));
        let s = nav
            .update_location(fix(&frame, 20.0, 0.0, 40, Some(90.0)))
            .unwrap();
        assert_eq!(s.current_step_index, 0);
        assert_eq!(s.progress, TripProgress::Navigating);
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
