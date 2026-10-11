//! Deterministic route simulator: drives a [`Route`] at a target speed and
//! emits noisy [`RawLocation`]s, with optional planned deviations and signal
//! degradation. Same seed, same fixes, on every platform.
//!
//! Used by the demo apps, by tests and by the fixture generator, so one
//! model of "a noisy phone driving this route" exists in the repo.

use core::f64::consts::{PI, TAU};

use crate::geo::{bearing_rad_to_deg, Enu, LocalFrame, RouteIndex};
use crate::types::{GeoPoint, NavError, RawLocation, Route};

/// xorshift64\* — small, deterministic, dependency-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator. A zero seed is replaced by a fixed non-zero one,
    /// since xorshift never leaves state zero.
    pub const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
    }
}

/// Simulator tunables. Defaults model a car at 50 km/h with typical phone GNSS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulatorConfig {
    /// Initial target speed. Metres/second.
    pub speed_mps: f64,
    /// Time step used by [`RouteSimulator::next_fix`]. Seconds.
    pub fix_interval_s: f64,
    /// Position noise, 1σ per axis. Metres.
    pub position_sigma_m: f64,
    /// Accuracy written into each fix. Metres.
    pub reported_accuracy_m: f64,
    /// Speed noise, 1σ. Metres/second.
    pub speed_sigma_mps: f64,
    /// Course noise, 1σ. Degrees.
    pub course_sigma_deg: f64,
    /// Random seed. Same seed, same fixes.
    pub seed: u64,
    /// Timestamp of the first fix. Milliseconds since the Unix epoch.
    pub start_timestamp_ms: u64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            speed_mps: 13.9,
            fix_interval_s: 1.0,
            position_sigma_m: 8.0,
            reported_accuracy_m: 10.0,
            speed_sigma_mps: 0.5,
            course_sigma_deg: 5.0,
            seed: 1,
            start_timestamp_ms: 1_700_000_000_000,
        }
    }
}

impl SimulatorConfig {
    fn validate(&self) -> Result<(), NavError> {
        let non_negative = [
            ("speed_mps", self.speed_mps),
            ("position_sigma_m", self.position_sigma_m),
            ("speed_sigma_mps", self.speed_sigma_mps),
            ("course_sigma_deg", self.course_sigma_deg),
        ];
        for (name, v) in non_negative {
            if !v.is_finite() || v < 0.0 {
                return Err(NavError::InvalidConfig(format!(
                    "{name} must be non-negative"
                )));
            }
        }
        for (name, v) in [
            ("fix_interval_s", self.fix_interval_s),
            ("reported_accuracy_m", self.reported_accuracy_m),
        ] {
            if !v.is_finite() || v <= 0.0 {
                return Err(NavError::InvalidConfig(format!("{name} must be positive")));
            }
        }
        Ok(())
    }
}

/// Leave the route for a while: from `at_m` along the route, drive parallel
/// to it offset by `lateral_m` (positive = right of travel) for `length_m`,
/// ramping the offset in and out, then rejoin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannedDeviation {
    /// Route distance where the deviation starts. Metres.
    pub at_m: f64,
    /// Lateral offset at full deviation; positive is right of travel. Metres.
    pub lateral_m: f64,
    /// Route distance covered while deviated. Metres.
    pub length_m: f64,
}

/// Drives a route and emits noisy fixes. See the module docs.
#[derive(Debug, Clone)]
pub struct RouteSimulator {
    index: RouteIndex,
    frame: LocalFrame,
    config: SimulatorConfig,
    rng: Rng,
    along_m: f64,
    target_speed_mps: f64,
    timestamp_ms: u64,
    truth: Enu,
    deviation: Option<PlannedDeviation>,
    /// Multiplier on noise and reported accuracy while degraded.
    degradation: Option<f64>,
    fixes_emitted: u64,
}

impl RouteSimulator {
    /// Build a simulator at the start of `route`.
    pub fn new(route: &Route, config: SimulatorConfig) -> Result<Self, NavError> {
        config.validate()?;
        route.validate()?;
        let index = RouteIndex::new(&route.geometry)?;
        let frame = *index.frame();
        let truth = index.point_at_distance(0.0);
        Ok(Self {
            index,
            frame,
            rng: Rng::new(config.seed),
            config,
            along_m: 0.0,
            target_speed_mps: config.speed_mps,
            timestamp_ms: config.start_timestamp_ms,
            truth,
            deviation: None,
            degradation: None,
            fixes_emitted: 0,
        })
    }

    /// The configuration in use.
    pub fn config(&self) -> &SimulatorConfig {
        &self.config
    }

    /// Rewind to the start with the original seed; planned deviation and
    /// degradation settings are kept.
    pub fn reset(&mut self) {
        self.rng = Rng::new(self.config.seed);
        self.along_m = 0.0;
        self.target_speed_mps = self.config.speed_mps;
        self.timestamp_ms = self.config.start_timestamp_ms;
        self.truth = self.index.point_at_distance(0.0);
        self.fixes_emitted = 0;
    }

    /// Distance driven along the route so far. Metres.
    pub fn distance_along_m(&self) -> f64 {
        self.along_m
    }

    /// Total route length. Metres.
    pub fn route_length_m(&self) -> f64 {
        self.index.total_length_m()
    }

    /// Timestamp the next fix will carry (before adding `dt`). Milliseconds.
    pub fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Fixes emitted since the last reset.
    pub fn fixes_emitted(&self) -> u64 {
        self.fixes_emitted
    }

    /// True once the vehicle has reached the end of the route. Further fixes
    /// sit at the destination with zero speed.
    pub fn is_finished(&self) -> bool {
        self.along_m >= self.index.total_length_m()
    }

    /// True position of the vehicle at the last emitted fix (noise-free).
    pub fn truth(&self) -> GeoPoint {
        self.frame.to_geo(self.truth)
    }

    /// Current target speed. Metres/second.
    pub fn target_speed_mps(&self) -> f64 {
        self.target_speed_mps
    }

    /// Change the target speed. Negative or non-finite values are treated as zero.
    pub fn set_target_speed(&mut self, speed_mps: f64) {
        self.target_speed_mps = if speed_mps.is_finite() {
            speed_mps.max(0.0)
        } else {
            0.0
        };
    }

    /// Schedule (or replace) a planned deviation.
    pub fn set_planned_deviation(&mut self, deviation: PlannedDeviation) {
        self.deviation = Some(deviation);
    }

    /// Remove the planned deviation. Takes effect at the next fix.
    pub fn clear_planned_deviation(&mut self) {
        self.deviation = None;
    }

    /// Multiply position noise and reported accuracy by `sigma_multiplier`
    /// (≥ 1) until [`stop_signal_degradation`](Self::stop_signal_degradation).
    pub fn start_signal_degradation(&mut self, sigma_multiplier: f64) {
        self.degradation = Some(if sigma_multiplier.is_finite() {
            sigma_multiplier.max(1.0)
        } else {
            1.0
        });
    }

    /// Restore nominal signal quality.
    pub fn stop_signal_degradation(&mut self) {
        self.degradation = None;
    }

    /// True while [`start_signal_degradation`](Self::start_signal_degradation) is in effect.
    pub fn is_signal_degraded(&self) -> bool {
        self.degradation.is_some()
    }

    /// Advance by the configured fix interval and emit a fix.
    pub fn next_fix(&mut self) -> RawLocation {
        self.advance(self.config.fix_interval_s)
    }

    /// Advance by `dt_s` seconds at the target speed and emit a fix.
    /// Non-positive or non-finite `dt_s` emits a fix without moving.
    pub fn advance(&mut self, dt_s: f64) -> RawLocation {
        let dt_s = if dt_s.is_finite() { dt_s.max(0.0) } else { 0.0 };
        let total = self.index.total_length_m();
        let speed = if self.along_m >= total {
            0.0
        } else {
            self.target_speed_mps
        };
        self.along_m = (self.along_m + speed * dt_s).min(total);
        self.timestamp_ms = self
            .timestamp_ms
            .saturating_add((dt_s * 1000.0).round() as u64);

        let on_route = self.index.point_at_distance(self.along_m);
        let segment = self.index.segment_at_distance(self.along_m);
        let bearing_rad = self.index.segment_bearing_rad(segment);
        let truth = match self.deviation {
            Some(d) => offset_point(on_route, bearing_rad, d, self.along_m),
            None => on_route,
        };
        self.truth = truth;

        let factor = self.degradation.unwrap_or(1.0);
        let sigma = self.config.position_sigma_m * factor;
        let noisy = Enu::new(
            truth.x + sigma * self.rng.gauss(),
            truth.y + sigma * self.rng.gauss(),
        );
        let reported_speed = if speed > 0.0 {
            (speed + self.config.speed_sigma_mps * self.rng.gauss()).max(0.0)
        } else {
            0.0
        };
        let course_deg = if speed > 0.0 {
            let jitter_rad = self.config.course_sigma_deg.to_radians() * self.rng.gauss();
            Some(bearing_rad_to_deg(bearing_rad + jitter_rad))
        } else {
            None
        };
        self.fixes_emitted += 1;
        RawLocation {
            point: self.frame.to_geo(noisy),
            accuracy_m: Some(self.config.reported_accuracy_m * factor),
            speed_mps: Some(reported_speed),
            course_deg,
            timestamp_ms: self.timestamp_ms,
        }
    }
}

/// Where the vehicle really is while a planned deviation is active: the
/// on-route point pushed sideways, with the offset ramping in over the
/// first and out over the last quarter (at most 20 m each).
fn offset_point(on_route: Enu, bearing_rad: f64, d: PlannedDeviation, along_m: f64) -> Enu {
    if d.length_m <= 0.0 || along_m < d.at_m || along_m > d.at_m + d.length_m {
        return on_route;
    }
    let ramp_m = (d.length_m / 4.0).clamp(f64::EPSILON, 20.0);
    let into = (along_m - d.at_m) / ramp_m;
    let out_of = (d.at_m + d.length_m - along_m) / ramp_m;
    let ramp = into.min(out_of).min(1.0);
    // Right-hand normal of the travel direction (sin b, cos b).
    let right = Enu::new(
        (bearing_rad + PI / 2.0).sin(),
        (bearing_rad + PI / 2.0).cos(),
    );
    Enu::new(
        on_route.x + right.x * d.lateral_m * ramp,
        on_route.y + right.y * d.lateral_m * ramp,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ManeuverType, RouteStep};

    /// Straight 1 km east, 100 m vertex spacing, two steps.
    fn straight_route() -> Route {
        let frame = LocalFrame::new(GeoPoint::new(45.0, 7.0));
        let geometry: Vec<GeoPoint> = (0..=10)
            .map(|i| frame.to_geo(Enu::new(i as f64 * 100.0, 0.0)))
            .collect();
        Route {
            geometry,
            steps: vec![
                RouteStep {
                    instruction: "Head east".into(),
                    maneuver: ManeuverType::Depart,
                    start_index: 0,
                    end_index: 10,
                    distance_m: 1000.0,
                },
                RouteStep {
                    instruction: "Arrive".into(),
                    maneuver: ManeuverType::Arrive,
                    start_index: 10,
                    end_index: 10,
                    distance_m: 0.0,
                },
            ],
        }
    }

    #[test]
    fn rng_is_deterministic_and_zero_seed_is_safe() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.uniform(), b.uniform());
        }
        let mut z = Rng::new(0);
        let u = z.uniform();
        assert!((0.0..1.0).contains(&u));
        assert_ne!(z.uniform(), u);
    }

    #[test]
    fn rejects_bad_config() {
        let route = straight_route();
        let cfg = SimulatorConfig {
            fix_interval_s: 0.0,
            ..SimulatorConfig::default()
        };
        assert!(matches!(
            RouteSimulator::new(&route, cfg),
            Err(NavError::InvalidConfig(_))
        ));
    }

    #[test]
    fn same_seed_same_fixes() {
        let route = straight_route();
        let cfg = SimulatorConfig::default();
        let mut a = RouteSimulator::new(&route, cfg).unwrap();
        let mut b = RouteSimulator::new(&route, cfg).unwrap();
        for _ in 0..50 {
            assert_eq!(a.next_fix(), b.next_fix());
        }
        let mut c = RouteSimulator::new(&route, SimulatorConfig { seed: 2, ..cfg }).unwrap();
        a.reset();
        assert_ne!(a.next_fix(), c.next_fix());
    }

    #[test]
    fn drives_to_the_end_and_stops() {
        let route = straight_route();
        let cfg = SimulatorConfig {
            speed_mps: 10.0,
            position_sigma_m: 0.0,
            speed_sigma_mps: 0.0,
            course_sigma_deg: 0.0,
            ..SimulatorConfig::default()
        };
        let mut sim = RouteSimulator::new(&route, cfg).unwrap();
        let mut n = 0;
        while !sim.is_finished() {
            let fix = sim.next_fix();
            n += 1;
            assert_eq!(fix.speed_mps, Some(10.0));
            assert!((fix.course_deg.unwrap() - 90.0).abs() < 1e-6);
            assert!(n < 1000);
        }
        assert_eq!(n, 100);
        assert!((sim.distance_along_m() - 1000.0).abs() < 1e-9);
        let parked = sim.next_fix();
        assert_eq!(parked.speed_mps, Some(0.0));
        assert_eq!(parked.course_deg, None);
        assert_eq!(parked.timestamp_ms, cfg.start_timestamp_ms + 101_000);
        let end = sim.truth();
        let last = route.geometry.last().unwrap();
        assert!((end.lat - last.lat).abs() < 1e-9 && (end.lng - last.lng).abs() < 1e-9);
    }

    #[test]
    fn planned_deviation_offsets_truth_then_rejoins() {
        let route = straight_route();
        let cfg = SimulatorConfig {
            speed_mps: 10.0,
            position_sigma_m: 0.0,
            ..SimulatorConfig::default()
        };
        let mut sim = RouteSimulator::new(&route, cfg).unwrap();
        sim.set_planned_deviation(PlannedDeviation {
            at_m: 200.0,
            lateral_m: 60.0,
            length_m: 200.0,
        });
        let frame = LocalFrame::new(route.geometry[0]);
        let mut max_offset: f64 = 0.0;
        let mut offsets_after = Vec::new();
        for k in 1..=60 {
            let fix = sim.next_fix();
            let e = frame.to_enu(fix.point);
            // Heading east, "right" is south: negative y.
            if (20..=40).contains(&k) {
                max_offset = max_offset.max(-e.y);
            }
            if k > 45 {
                offsets_after.push(e.y.abs());
            }
        }
        assert!((max_offset - 60.0).abs() < 1e-6, "max offset {max_offset}");
        assert!(offsets_after.iter().all(|o| *o < 1e-6));
    }

    #[test]
    fn signal_degradation_scales_accuracy() {
        let route = straight_route();
        let mut sim = RouteSimulator::new(&route, SimulatorConfig::default()).unwrap();
        assert_eq!(sim.next_fix().accuracy_m, Some(10.0));
        sim.start_signal_degradation(3.0);
        assert!(sim.is_signal_degraded());
        assert_eq!(sim.next_fix().accuracy_m, Some(30.0));
        sim.stop_signal_degradation();
        assert_eq!(sim.next_fix().accuracy_m, Some(10.0));
    }

    #[test]
    fn speed_changes_apply_to_the_next_fix() {
        let route = straight_route();
        let cfg = SimulatorConfig {
            position_sigma_m: 0.0,
            speed_sigma_mps: 0.0,
            ..SimulatorConfig::default()
        };
        let mut sim = RouteSimulator::new(&route, cfg).unwrap();
        sim.set_target_speed(5.0);
        sim.advance(2.0);
        assert!((sim.distance_along_m() - 10.0).abs() < 1e-9);
        sim.set_target_speed(-3.0);
        sim.advance(1.0);
        assert!((sim.distance_along_m() - 10.0).abs() < 1e-9);
        assert_eq!(sim.fixes_emitted(), 2);
    }
}
