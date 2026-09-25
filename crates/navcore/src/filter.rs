//! Constant-velocity Kalman filter over a local ENU frame.
//!
//! State is `[x, y, vx, vy]` (metres, metres/second). Position is always
//! measured; when the fix carries speed *and* course, velocity is measured
//! too, which is what keeps the estimate honest through turns.
//!
//! Fixes are rejected (not incorporated) when they are not newer than the
//! last accepted fix or when they imply an impossible speed. After
//! `max_consecutive_rejects` implausible fixes in a row the filter assumes it
//! is the one that is lost and re-initialises on the next fix.

use crate::geo::{deg_to_rad, normalize_bearing_rad, Enu};
use crate::types::{NavError, RawLocation};

/// Tunables for [`LocationFilter`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterConfig {
    /// Process noise: standard deviation of unmodelled acceleration, m/s².
    /// Lower = smoother but laggier through turns.
    pub process_noise_accel_mps2: f64,
    /// Used when the fix has no accuracy or a non-positive one, metres (1σ).
    pub default_accuracy_m: f64,
    /// Floor on the position measurement noise, metres (1σ). Providers
    /// routinely over-report their accuracy.
    pub min_accuracy_m: f64,
    /// Measurement noise for reported velocity, m/s (1σ).
    pub velocity_noise_mps: f64,
    /// Fixes implying a speed above this relative to the last accepted fix
    /// are rejected as teleports, m/s.
    pub max_speed_mps: f64,
    /// Below this estimated speed the heading is held rather than derived
    /// from a noisy near-zero velocity, m/s.
    pub min_heading_speed_mps: f64,
    /// After this many consecutive implausible fixes the filter resets.
    pub max_consecutive_rejects: u32,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            process_noise_accel_mps2: 1.0,
            default_accuracy_m: 10.0,
            min_accuracy_m: 3.0,
            velocity_noise_mps: 1.0,
            max_speed_mps: 70.0,
            min_heading_speed_mps: 1.0,
            max_consecutive_rejects: 5,
        }
    }
}

/// Filter output for one accepted fix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilteredLocation {
    /// Estimated position in the frame, metres.
    pub position: Enu,
    /// Estimated velocity, m/s east (`x`) and north (`y`).
    pub velocity: Enu,
    /// Estimated ground speed, m/s.
    pub speed_mps: f64,
    /// Estimated heading, radians clockwise from north, `[0, 2π)`. Held from
    /// the last confident value when nearly stationary; `None` until a
    /// heading has ever been established.
    pub heading_rad: Option<f64>,
    /// Position uncertainty (1σ, isotropic approximation), metres.
    pub position_std_m: f64,
    /// Timestamp of the fix this estimate corresponds to.
    pub timestamp_ms: u64,
}

type Mat4 = [[f64; 4]; 4];

/// Constant-velocity Kalman filter. See the module docs.
#[derive(Debug, Clone)]
pub struct LocationFilter {
    config: FilterConfig,
    /// `[x, y, vx, vy]`.
    x: [f64; 4],
    p: Mat4,
    last: Option<FilteredLocation>,
    held_heading_rad: Option<f64>,
    consecutive_rejects: u32,
}

impl LocationFilter {
    /// A filter with no state; the first accepted fix initialises it.
    pub fn new(config: FilterConfig) -> Self {
        Self {
            config,
            x: [0.0; 4],
            p: [[0.0; 4]; 4],
            last: None,
            held_heading_rad: None,
            consecutive_rejects: 0,
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &FilterConfig {
        &self.config
    }

    /// Drop all state; the next fix re-initialises.
    pub fn reset(&mut self) {
        self.last = None;
        self.held_heading_rad = None;
        self.consecutive_rejects = 0;
    }

    /// Most recent accepted estimate.
    pub fn last(&self) -> Option<&FilteredLocation> {
        self.last.as_ref()
    }

    /// Incorporate a fix. `point` is `raw.point` already projected into the
    /// frame; the caller owns the frame so the filter stays trig-free.
    pub fn update(&mut self, point: Enu, raw: &RawLocation) -> Result<FilteredLocation, NavError> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NavError::InvalidLocation);
        }
        let acc = self.measurement_sigma(raw);
        let measured_velocity = Self::measured_velocity(raw);

        let Some(last) = self.last else {
            return Ok(self.initialise(point, raw, acc, measured_velocity));
        };

        if raw.timestamp_ms <= last.timestamp_ms {
            return Err(NavError::StaleLocation {
                timestamp_ms: raw.timestamp_ms,
                last_timestamp_ms: last.timestamp_ms,
            });
        }
        let dt = (raw.timestamp_ms - last.timestamp_ms) as f64 / 1000.0;

        // Teleport check against the last estimate, allowing for the
        // uncertainty of both the estimate and the new measurement.
        let jump = last.position.distance_to(point);
        let slack = 2.0 * (last.position_std_m + acc);
        let implied_speed = ((jump - slack).max(0.0)) / dt;
        if implied_speed > self.config.max_speed_mps {
            self.consecutive_rejects += 1;
            if self.consecutive_rejects >= self.config.max_consecutive_rejects {
                // We are the ones who are lost. Start over from this fix.
                self.reset();
                return Ok(self.initialise(point, raw, acc, measured_velocity));
            }
            return Err(NavError::ImplausibleLocation {
                implied_speed_mps: implied_speed,
                max_speed_mps: self.config.max_speed_mps,
            });
        }
        self.consecutive_rejects = 0;

        self.predict(dt);
        self.correct_pair(0, 1, [point.x, point.y], acc * acc);
        if let Some(v) = measured_velocity {
            let r = self.config.velocity_noise_mps * self.config.velocity_noise_mps;
            self.correct_pair(2, 3, [v.x, v.y], r);
        }
        Ok(self.emit(raw.timestamp_ms))
    }

    fn measurement_sigma(&self, raw: &RawLocation) -> f64 {
        match raw.accuracy_m {
            Some(a) if a.is_finite() && a > 0.0 => a.max(self.config.min_accuracy_m),
            _ => self.config.default_accuracy_m,
        }
    }

    /// Velocity vector from reported speed + course, if both are usable.
    fn measured_velocity(raw: &RawLocation) -> Option<Enu> {
        let speed = raw.speed_mps.filter(|s| s.is_finite() && *s >= 0.0)?;
        let course = raw.course_deg.filter(|c| c.is_finite())?;
        let c = deg_to_rad(course);
        Some(Enu::new(speed * c.sin(), speed * c.cos()))
    }

    fn initialise(
        &mut self,
        point: Enu,
        raw: &RawLocation,
        acc: f64,
        velocity: Option<Enu>,
    ) -> FilteredLocation {
        let v = velocity.unwrap_or_default();
        self.x = [point.x, point.y, v.x, v.y];
        self.p = [[0.0; 4]; 4];
        self.p[0][0] = acc * acc;
        self.p[1][1] = acc * acc;
        // Unknown velocity: anything up to max speed is plausible.
        let v_var = if velocity.is_some() {
            let s = self.config.velocity_noise_mps;
            s * s
        } else {
            let s = self.config.max_speed_mps * 0.5;
            s * s
        };
        self.p[2][2] = v_var;
        self.p[3][3] = v_var;
        self.consecutive_rejects = 0;
        self.emit(raw.timestamp_ms)
    }

    /// Advance the state by `dt` seconds under the constant-velocity model.
    fn predict(&mut self, dt: f64) {
        // x' = F x
        self.x[0] += self.x[2] * dt;
        self.x[1] += self.x[3] * dt;

        // P' = F P Fᵀ + Q, with F = [[I, dt·I], [0, I]].
        let p = self.p;
        let mut fp = p;
        for c in 0..4 {
            fp[0][c] = p[0][c] + dt * p[2][c];
            fp[1][c] = p[1][c] + dt * p[3][c];
        }
        let mut fpft = fp;
        for r in 0..4 {
            fpft[r][0] = fp[r][0] + dt * fp[r][2];
            fpft[r][1] = fp[r][1] + dt * fp[r][3];
        }

        let q = self.config.process_noise_accel_mps2 * self.config.process_noise_accel_mps2;
        let dt2 = dt * dt;
        let dt3 = dt2 * dt;
        let dt4 = dt3 * dt;
        fpft[0][0] += q * dt4 / 4.0;
        fpft[1][1] += q * dt4 / 4.0;
        fpft[0][2] += q * dt3 / 2.0;
        fpft[2][0] += q * dt3 / 2.0;
        fpft[1][3] += q * dt3 / 2.0;
        fpft[3][1] += q * dt3 / 2.0;
        fpft[2][2] += q * dt2;
        fpft[3][3] += q * dt2;
        self.p = fpft;
    }

    /// Kalman correction for a 2-D measurement of state components `(i, j)`
    /// with isotropic measurement variance `r`.
    fn correct_pair(&mut self, i: usize, j: usize, z: [f64; 2], r: f64) {
        let p = self.p;
        // S = H P Hᵀ + R  (2×2)
        let s00 = p[i][i] + r;
        let s01 = p[i][j];
        let s10 = p[j][i];
        let s11 = p[j][j] + r;
        let det = s00 * s11 - s01 * s10;
        if det.abs() < 1e-12 || !det.is_finite() {
            return;
        }
        let inv = [[s11 / det, -s01 / det], [-s10 / det, s00 / det]];

        // K = P Hᵀ S⁻¹  (4×2); P Hᵀ is columns i and j of P.
        let mut k = [[0.0; 2]; 4];
        for (row, k_row) in k.iter_mut().enumerate() {
            let phi = p[row][i];
            let phj = p[row][j];
            k_row[0] = phi * inv[0][0] + phj * inv[1][0];
            k_row[1] = phi * inv[0][1] + phj * inv[1][1];
        }

        // x += K (z − H x)
        let y0 = z[0] - self.x[i];
        let y1 = z[1] - self.x[j];
        for (row, k_row) in k.iter().enumerate() {
            self.x[row] += k_row[0] * y0 + k_row[1] * y1;
        }

        // P = (I − K H) P ; (K H) has columns i and j equal to K's columns.
        let mut np = p;
        for (row, k_row) in k.iter().enumerate() {
            for c in 0..4 {
                np[row][c] -= k_row[0] * p[i][c] + k_row[1] * p[j][c];
            }
        }
        // Symmetrise to fight drift.
        for r_ in 0..4 {
            for c in (r_ + 1)..4 {
                let m = 0.5 * (np[r_][c] + np[c][r_]);
                np[r_][c] = m;
                np[c][r_] = m;
            }
        }
        self.p = np;
    }

    fn emit(&mut self, timestamp_ms: u64) -> FilteredLocation {
        let velocity = Enu::new(self.x[2], self.x[3]);
        let speed = (velocity.x * velocity.x + velocity.y * velocity.y).sqrt();
        if speed >= self.config.min_heading_speed_mps {
            self.held_heading_rad = Some(normalize_bearing_rad(velocity.x.atan2(velocity.y)));
        }
        let out = FilteredLocation {
            position: Enu::new(self.x[0], self.x[1]),
            velocity,
            speed_mps: speed,
            heading_rad: self.held_heading_rad,
            position_std_m: (0.5 * (self.p[0][0] + self.p[1][1])).max(0.0).sqrt(),
            timestamp_ms,
        };
        self.last = Some(out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GeoPoint;

    /// Tiny deterministic PRNG (xorshift64*) so tests need no crates.
    struct Rng(u64);
    impl Rng {
        fn next_f64(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        }
        fn gauss(&mut self) -> f64 {
            let u1 = self.next_f64().max(1e-12);
            let u2 = self.next_f64();
            (-2.0 * u1.ln()).sqrt() * (core::f64::consts::TAU * u2).cos()
        }
    }

    fn fix(ts: u64, acc: Option<f64>) -> RawLocation {
        RawLocation {
            point: GeoPoint::default(),
            accuracy_m: acc,
            speed_mps: None,
            course_deg: None,
            timestamp_ms: ts,
        }
    }

    #[test]
    fn first_fix_initialises() {
        let mut f = LocationFilter::new(FilterConfig::default());
        let out = f.update(Enu::new(5.0, 6.0), &fix(1000, Some(8.0))).unwrap();
        assert_eq!(out.position, Enu::new(5.0, 6.0));
        assert_eq!(out.speed_mps, 0.0);
        assert_eq!(out.heading_rad, None);
        assert!((out.position_std_m - 8.0).abs() < 1e-9);
    }

    #[test]
    fn stale_fix_rejected() {
        let mut f = LocationFilter::new(FilterConfig::default());
        f.update(Enu::new(0.0, 0.0), &fix(1000, None)).unwrap();
        let e = f.update(Enu::new(1.0, 0.0), &fix(1000, None));
        assert!(matches!(e, Err(NavError::StaleLocation { .. })));
        let e = f.update(Enu::new(1.0, 0.0), &fix(500, None));
        assert!(matches!(e, Err(NavError::StaleLocation { .. })));
        // State untouched.
        assert_eq!(f.last().unwrap().position, Enu::new(0.0, 0.0));
    }

    #[test]
    fn teleport_rejected_then_reset_after_repeats() {
        let cfg = FilterConfig {
            max_consecutive_rejects: 3,
            ..FilterConfig::default()
        };
        let mut f = LocationFilter::new(cfg);
        f.update(Enu::new(0.0, 0.0), &fix(1000, Some(5.0))).unwrap();
        // 5 km in one second.
        let e = f.update(Enu::new(5000.0, 0.0), &fix(2000, Some(5.0)));
        assert!(matches!(e, Err(NavError::ImplausibleLocation { .. })));
        let e = f.update(Enu::new(5000.0, 0.0), &fix(3000, Some(5.0)));
        assert!(matches!(e, Err(NavError::ImplausibleLocation { .. })));
        // Third strike: filter gives up and re-initialises on the new fix.
        let out = f
            .update(Enu::new(5000.0, 0.0), &fix(4000, Some(5.0)))
            .unwrap();
        assert_eq!(out.position, Enu::new(5000.0, 0.0));
    }

    #[test]
    fn plausible_fast_fix_accepted() {
        let mut f = LocationFilter::new(FilterConfig::default());
        f.update(Enu::new(0.0, 0.0), &fix(1000, Some(5.0))).unwrap();
        // 30 m/s is a normal road speed.
        assert!(f.update(Enu::new(30.0, 0.0), &fix(2000, Some(5.0))).is_ok());
    }

    #[test]
    fn converges_on_straight_line() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let sigma = 10.0;
        let speed = 15.0;
        let mut f = LocationFilter::new(FilterConfig::default());

        let mut raw_sq = 0.0;
        let mut filt_sq = 0.0;
        let mut n = 0;
        let mut converged_at = None;
        // Long run so the sample RMS is close to the steady-state value.
        // Position-only measurements at tracking index Λ = σ_a·dt²/σ_r = 0.1
        // give a theoretical filtered/raw std ratio of ≈ 0.54.
        for k in 0..2000u64 {
            let truth = Enu::new(speed * k as f64, 0.0);
            let meas = Enu::new(truth.x + sigma * rng.gauss(), truth.y + sigma * rng.gauss());
            let out = f.update(meas, &fix(1000 + k * 1000, Some(sigma))).unwrap();
            let v_err = (out.velocity.x - speed).abs() + out.velocity.y.abs();
            if converged_at.is_none() && v_err < 1.5 {
                converged_at = Some(k);
            }
            if k >= 50 {
                raw_sq += meas.distance_sq_to(truth);
                filt_sq += out.position.distance_sq_to(truth);
                n += 1;
            }
        }
        let raw_rms = (raw_sq / n as f64).sqrt();
        let filt_rms = (filt_sq / n as f64).sqrt();
        assert!(
            converged_at.is_some_and(|k| k <= 20),
            "velocity converged at {converged_at:?}"
        );
        assert!(
            filt_rms < 0.6 * raw_rms,
            "filtered rms {filt_rms:.2} vs raw {raw_rms:.2}"
        );
        let last = f.last().unwrap();
        assert!((last.heading_rad.unwrap() - core::f64::consts::FRAC_PI_2).abs() < 0.1);
    }

    #[test]
    fn velocity_measurement_speeds_convergence() {
        let speed = 12.0;
        let run = |with_velocity: bool| {
            let mut f = LocationFilter::new(FilterConfig::default());
            let mut out = None;
            for k in 0..3u64 {
                let mut r = fix(1000 + k * 1000, Some(10.0));
                if with_velocity {
                    r.speed_mps = Some(speed);
                    r.course_deg = Some(0.0); // north
                }
                out = Some(f.update(Enu::new(0.0, speed * k as f64), &r).unwrap());
            }
            out.unwrap()
        };
        let with = run(true);
        let without = run(false);
        assert!((with.velocity.y - speed).abs() < (without.velocity.y - speed).abs());
        assert!((with.velocity.y - speed).abs() < 0.5);
    }

    #[test]
    fn heading_held_when_stationary() {
        let mut f = LocationFilter::new(FilterConfig::default());
        let mut r = fix(1000, Some(5.0));
        r.speed_mps = Some(10.0);
        r.course_deg = Some(90.0);
        f.update(Enu::new(0.0, 0.0), &r).unwrap();
        let h0 = f.last().unwrap().heading_rad.unwrap();
        // Now stop: velocity measured zero for a while.
        for k in 1..10u64 {
            let mut r = fix(1000 + k * 1000, Some(5.0));
            r.speed_mps = Some(0.0);
            r.course_deg = Some(0.0);
            f.update(Enu::new(10.0, 0.0), &r).unwrap();
        }
        let last = f.last().unwrap();
        assert!(last.speed_mps < 1.0);
        assert_eq!(last.heading_rad, Some(h0));
    }

    #[test]
    fn covariance_stays_symmetric_positive() {
        let mut f = LocationFilter::new(FilterConfig::default());
        let mut ts = 1000u64;
        for k in 0..500u64 {
            ts += if k % 7 == 0 { 5000 } else { 1000 };
            f.update(
                Enu::new(k as f64 * 3.0, (k as f64 * 0.1).sin() * 20.0),
                &fix(ts, Some(7.0)),
            )
            .unwrap();
        }
        for r in 0..4 {
            assert!(f.p[r][r] > 0.0);
            for c in 0..4 {
                assert!((f.p[r][c] - f.p[c][r]).abs() < 1e-6);
            }
        }
    }
}
