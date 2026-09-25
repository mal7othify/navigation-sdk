//! Map matching restricted to the route polyline.
//!
//! [`HmmMatcher`] is a Newson–Krumm style hidden Markov model over a bounded
//! window of recent fixes: emission is a Gaussian on cross-track distance,
//! transition penalises disagreement between straight-line and along-route
//! distance, heading agreement is folded into the per-candidate score, and
//! moving backwards along the route is penalised so U-turn routes (where the
//! polyline itself doubles back) resolve correctly.
//!
//! [`SimpleMatcher`] snaps to the nearest segment and exists so tests can
//! show what the HMM buys.
//!
//! All per-fix work uses fixed-capacity storage owned by the matcher; the
//! hot path allocates nothing once warmed up.

use crate::geo::{bearing_diff_rad, Enu, RouteIndex, RouteProjection};
use crate::types::NavError;

/// Maximum candidates kept per fix. Denser sets are truncated to the closest.
pub const MAX_CANDIDATES: usize = 8;
/// Maximum window length; `MatcherConfig::window_size` is clamped to this.
pub const MAX_WINDOW: usize = 16;

/// Tunables for [`HmmMatcher`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatcherConfig {
    /// Search radius for candidate segments, metres.
    pub candidate_radius_m: f64,
    /// Radius tried once when the first search finds nothing, metres.
    pub widened_radius_m: f64,
    /// Number of recent fixes the Viterbi pass runs over (≤ [`MAX_WINDOW`]).
    pub window_size: usize,
    /// Floor on the emission σ, metres. Providers over-report accuracy.
    pub min_emission_sigma_m: f64,
    /// Transition scale β, metres: `log p = -|d_straight - d_route| / β`.
    /// Scaled by the time gap between fixes in seconds (min 1).
    pub transition_beta_m: f64,
    /// Weight of the heading term `-w · (1 - cos Δθ)`; 0 disables it.
    pub heading_weight: f64,
    /// Moving further backwards along the route than this, either between
    /// consecutive fixes or relative to the previous match, is penalised. Metres.
    pub max_backtrack_m: f64,
    /// Flat log-probability penalty for such backtracking.
    pub backtrack_penalty: f64,
}

impl Default for MatcherConfig {
    fn default() -> Self {
        Self {
            candidate_radius_m: 50.0,
            widened_radius_m: 150.0,
            window_size: 10,
            min_emission_sigma_m: 5.0,
            transition_beta_m: 10.0,
            heading_weight: 3.0,
            max_backtrack_m: 30.0,
            backtrack_penalty: 10.0,
        }
    }
}

/// One fix as the matcher sees it (already in the route's local frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchInput {
    /// Position in the [`RouteIndex`] frame, metres.
    pub position: Enu,
    /// Position uncertainty (1σ), metres.
    pub sigma_m: f64,
    /// Heading, radians clockwise from north, if known and trustworthy.
    pub heading_rad: Option<f64>,
    /// Fix time, milliseconds.
    pub timestamp_ms: u64,
}

/// Anything that turns a fix into a position on the route.
pub trait Matcher {
    /// Match one fix. Never fails for finite input: with no candidates in
    /// range the nearest segment is returned and its large
    /// `distance_from_route_m` lets the caller decide it is off-route.
    fn match_fix(
        &mut self,
        index: &RouteIndex,
        input: &MatchInput,
    ) -> Result<RouteProjection, NavError>;

    /// Forget all history (e.g. after the route changes).
    fn reset(&mut self);
}

/// Nearest-segment matcher. No memory, no heading.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleMatcher;

impl Matcher for SimpleMatcher {
    fn match_fix(
        &mut self,
        index: &RouteIndex,
        input: &MatchInput,
    ) -> Result<RouteProjection, NavError> {
        if !input.position.x.is_finite() || !input.position.y.is_finite() {
            return Err(NavError::InvalidLocation);
        }
        Ok(index.nearest_segment(input.position))
    }

    fn reset(&mut self) {}
}

#[derive(Debug, Clone, Copy, Default)]
struct Candidate {
    proj: RouteProjection,
    /// Emission log-probability including heading and backtrack terms.
    log_emission: f64,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    position: Enu,
    timestamp_ms: u64,
    candidates: [Candidate; MAX_CANDIDATES],
    len: usize,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            position: Enu::default(),
            timestamp_ms: 0,
            candidates: [Candidate::default(); MAX_CANDIDATES],
            len: 0,
        }
    }
}

/// Windowed Viterbi map matcher. See the module docs.
#[derive(Debug, Clone)]
pub struct HmmMatcher {
    config: MatcherConfig,
    window: [Slot; MAX_WINDOW],
    /// Index of the newest slot; valid when `len > 0`.
    head: usize,
    len: usize,
    /// Along-route distance of the previous result, for the backtrack rule.
    last_along_m: Option<f64>,
    /// Reused for R-tree results.
    segment_scratch: Vec<usize>,
}

impl HmmMatcher {
    /// A matcher with an empty window.
    pub fn new(config: MatcherConfig) -> Self {
        let config = MatcherConfig {
            window_size: config.window_size.clamp(1, MAX_WINDOW),
            ..config
        };
        Self {
            config,
            window: [Slot::default(); MAX_WINDOW],
            head: 0,
            len: 0,
            last_along_m: None,
            segment_scratch: Vec::with_capacity(64),
        }
    }

    /// The configuration in use (with `window_size` clamped).
    pub fn config(&self) -> &MatcherConfig {
        &self.config
    }

    /// Number of fixes currently in the window.
    pub fn window_len(&self) -> usize {
        self.len
    }

    /// Fill `slot` with the best candidates for `input`. Always ≥ 1 candidate.
    fn collect_candidates(&mut self, index: &RouteIndex, input: &MatchInput, slot: &mut Slot) {
        slot.position = input.position;
        slot.timestamp_ms = input.timestamp_ms;
        slot.len = 0;

        index.segments_within(
            input.position,
            self.config.candidate_radius_m,
            &mut self.segment_scratch,
        );
        if self.segment_scratch.is_empty() {
            index.segments_within(
                input.position,
                self.config.widened_radius_m,
                &mut self.segment_scratch,
            );
        }

        let sigma = input.sigma_m.max(self.config.min_emission_sigma_m);
        if self.segment_scratch.is_empty() {
            let proj = index.nearest_segment(input.position);
            slot.candidates[0] = Candidate {
                proj,
                log_emission: self.emission(&proj, sigma, input.heading_rad),
            };
            slot.len = 1;
            return;
        }

        for &seg in &self.segment_scratch {
            let proj = index.project_onto(seg, input.position);
            let cand = Candidate {
                proj,
                log_emission: self.emission(&proj, sigma, input.heading_rad),
            };
            Self::insert_by_distance(slot, cand);
        }
    }

    /// Keep the `MAX_CANDIDATES` closest candidates, ordered by distance
    /// then segment index so ties resolve deterministically.
    fn insert_by_distance(slot: &mut Slot, cand: Candidate) {
        let key = |c: &Candidate| (c.proj.distance_from_route_m, c.proj.segment_index);
        let mut pos = slot.len;
        while pos > 0 {
            let prev = &slot.candidates[pos - 1];
            let (pd, pi) = key(prev);
            let (cd, ci) = key(&cand);
            if pd < cd || (pd == cd && pi <= ci) {
                break;
            }
            pos -= 1;
        }
        if pos >= MAX_CANDIDATES {
            return;
        }
        let end = slot.len.min(MAX_CANDIDATES - 1);
        let mut i = end;
        while i > pos {
            slot.candidates[i] = slot.candidates[i - 1];
            i -= 1;
        }
        slot.candidates[pos] = cand;
        slot.len = (slot.len + 1).min(MAX_CANDIDATES);
    }

    /// Evidence for one candidate: Gaussian on cross-track distance plus a
    /// heading-agreement term. Stored per slot, so it must not depend on the
    /// matcher's mutable history.
    fn emission(&self, proj: &RouteProjection, sigma: f64, heading: Option<f64>) -> f64 {
        let z = proj.distance_from_route_m / sigma;
        let mut log_p = -0.5 * z * z;
        if let Some(h) = heading {
            if self.config.heading_weight > 0.0 {
                let d = bearing_diff_rad(h, proj.bearing_rad);
                log_p -= self.config.heading_weight * (1.0 - d.cos());
            }
        }
        log_p
    }

    /// Single-shot hysteresis against the previous result: a candidate for the
    /// newest fix that sits more than `max_backtrack_m` behind the last match
    /// pays a flat penalty. Flat and applied only once so a window full of
    /// contradicting evidence can still overturn a wrong match.
    fn hysteresis(&self, proj: &RouteProjection) -> f64 {
        match self.last_along_m {
            Some(last) if last - proj.distance_along_route_m > self.config.max_backtrack_m => {
                -self.config.backtrack_penalty
            }
            _ => 0.0,
        }
    }

    /// Continuity between consecutive fixes: straight-line displacement should
    /// match the *signed* along-route displacement, so travelling backwards
    /// along the route costs more the further it goes, with an extra flat
    /// penalty for jumps beyond `max_backtrack_m`.
    fn transition(&self, from: &Slot, a: &Candidate, to: &Slot, b: &Candidate) -> f64 {
        let straight = from.position.distance_to(to.position);
        let along = b.proj.distance_along_route_m - a.proj.distance_along_route_m;
        let dt_s = (to.timestamp_ms.saturating_sub(from.timestamp_ms) as f64 / 1000.0).max(1.0);
        let beta = self.config.transition_beta_m * dt_s;
        let mut log_p = -(straight - along).abs() / beta;
        if along < -self.config.max_backtrack_m {
            log_p -= self.config.backtrack_penalty;
        }
        log_p
    }

    /// Ring-buffer index of the `k`-th oldest slot in the window.
    #[inline]
    fn slot_index(&self, k: usize) -> usize {
        (self.head + MAX_WINDOW + 1 + k - self.len) % MAX_WINDOW
    }

    fn push_slot(&mut self, slot: Slot) {
        if self.len == 0 {
            self.head = 0;
        } else {
            self.head = (self.head + 1) % MAX_WINDOW;
        }
        self.window[self.head] = slot;
        if self.len < self.config.window_size {
            self.len += 1;
        }
    }

    /// Forward pass over the window; returns the best candidate of the newest slot.
    fn viterbi(&self) -> RouteProjection {
        let mut prev = [0.0f64; MAX_CANDIDATES];
        let mut cur = [0.0f64; MAX_CANDIDATES];

        let first = &self.window[self.slot_index(0)];
        for (i, c) in first.candidates[..first.len].iter().enumerate() {
            prev[i] = c.log_emission;
        }

        for k in 1..self.len {
            let from = &self.window[self.slot_index(k - 1)];
            let to = &self.window[self.slot_index(k)];
            for (j, b) in to.candidates[..to.len].iter().enumerate() {
                let mut best = f64::NEG_INFINITY;
                for (i, a) in from.candidates[..from.len].iter().enumerate() {
                    let s = prev[i] + self.transition(from, a, to, b);
                    if s > best {
                        best = s;
                    }
                }
                cur[j] = best + b.log_emission;
            }
            // Renormalise so long windows cannot underflow.
            let max = cur[..to.len]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            for v in &mut cur[..to.len] {
                *v -= max;
            }
            prev = cur;
        }

        let newest = &self.window[self.head];
        for (i, c) in newest.candidates[..newest.len].iter().enumerate() {
            prev[i] += self.hysteresis(&c.proj);
        }
        let mut best_i = 0;
        for i in 1..newest.len {
            if prev[i] > prev[best_i] {
                best_i = i;
            }
        }
        newest.candidates[best_i].proj
    }
}

impl Matcher for HmmMatcher {
    fn match_fix(
        &mut self,
        index: &RouteIndex,
        input: &MatchInput,
    ) -> Result<RouteProjection, NavError> {
        if !input.position.x.is_finite() || !input.position.y.is_finite() {
            return Err(NavError::InvalidLocation);
        }
        let mut slot = Slot::default();
        self.collect_candidates(index, input, &mut slot);
        self.push_slot(slot);
        let best = self.viterbi();
        self.last_along_m = Some(best.distance_along_route_m);
        Ok(best)
    }

    fn reset(&mut self) {
        self.len = 0;
        self.head = 0;
        self.last_along_m = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GeoPoint;
    use core::f64::consts::{FRAC_PI_2, PI};

    /// Out-and-back: east 500 m on y=0, connector north 20 m, west 500 m on y=20.
    fn out_and_back() -> RouteIndex {
        let frame = crate::geo::LocalFrame::new(GeoPoint::new(50.0, 8.0));
        let mut pts = Vec::new();
        for i in 0..=10 {
            pts.push(Enu::new(i as f64 * 50.0, 0.0));
        }
        pts.push(Enu::new(500.0, 20.0));
        for i in (0..10).rev() {
            pts.push(Enu::new(i as f64 * 50.0, 20.0));
        }
        let geo: Vec<GeoPoint> = pts.into_iter().map(|e| frame.to_geo(e)).collect();
        RouteIndex::new(&geo).unwrap()
    }

    fn input(index: &RouteIndex, x: f64, y: f64, heading: Option<f64>, t: u64) -> MatchInput {
        // Route frame origin is the bbox centre; shift test coords into it.
        let origin = index.vertices()[0];
        MatchInput {
            position: Enu::new(origin.x + x, origin.y + y),
            sigma_m: 8.0,
            heading_rad: heading,
            timestamp_ms: t,
        }
    }

    #[test]
    fn simple_matcher_is_nearest() {
        let index = out_and_back();
        let mut m = SimpleMatcher;
        // 12 m north of the outbound leg is closer to the return leg (8 m away).
        let r = m
            .match_fix(&index, &input(&index, 250.0, 12.0, None, 0))
            .unwrap();
        assert!(r.distance_along_route_m > 520.0, "{r:?}");
    }

    #[test]
    fn hmm_uses_heading_to_pick_leg() {
        let index = out_and_back();
        let mut m = HmmMatcher::new(MatcherConfig::default());
        // Same ambiguous point, heading east → outbound leg.
        let r = m
            .match_fix(&index, &input(&index, 250.0, 12.0, Some(FRAC_PI_2), 0))
            .unwrap();
        assert!(r.distance_along_route_m < 500.0, "{r:?}");
        assert!((r.distance_along_route_m - 250.0).abs() < 1e-6);

        let mut m = HmmMatcher::new(MatcherConfig::default());
        // Heading west → return leg.
        let r = m
            .match_fix(&index, &input(&index, 250.0, 12.0, Some(1.5 * PI), 0))
            .unwrap();
        assert!(r.distance_along_route_m > 520.0, "{r:?}");
    }

    #[test]
    fn hmm_transition_keeps_leg_without_heading() {
        let index = out_and_back();
        let mut m = HmmMatcher::new(MatcherConfig::default());
        // Drive east along y≈0 with a noisy sample that strays to y=13.
        let ys = [0.0, 2.0, -3.0, 13.0, 1.0, -1.0];
        let mut alongs = Vec::new();
        for (k, y) in ys.iter().enumerate() {
            let r = m
                .match_fix(
                    &index,
                    &input(&index, 100.0 + 15.0 * k as f64, *y, None, k as u64 * 1000),
                )
                .unwrap();
            alongs.push(r.distance_along_route_m);
        }
        for (k, a) in alongs.iter().enumerate() {
            assert!(*a < 500.0, "fix {k} jumped to the return leg: {a}");
        }
        for w in alongs.windows(2) {
            assert!(
                w[1] > w[0],
                "along-route distance must increase: {alongs:?}"
            );
        }
    }

    #[test]
    fn falls_back_to_nearest_when_far_away() {
        let index = out_and_back();
        let mut m = HmmMatcher::new(MatcherConfig::default());
        let r = m
            .match_fix(&index, &input(&index, 250.0, 400.0, None, 0))
            .unwrap();
        assert!(r.distance_from_route_m > 300.0);
        assert_eq!(m.window_len(), 1);
    }

    #[test]
    fn window_is_bounded_and_reset_clears() {
        let index = out_and_back();
        let mut m = HmmMatcher::new(MatcherConfig {
            window_size: 4,
            ..MatcherConfig::default()
        });
        for k in 0..20u64 {
            m.match_fix(&index, &input(&index, 10.0 * k as f64, 0.0, None, k * 1000))
                .unwrap();
            assert!(m.window_len() <= 4);
        }
        assert_eq!(m.window_len(), 4);
        m.reset();
        assert_eq!(m.window_len(), 0);
        assert_eq!(m.last_along_m, None);
    }

    #[test]
    fn window_size_is_clamped() {
        let m = HmmMatcher::new(MatcherConfig {
            window_size: 999,
            ..MatcherConfig::default()
        });
        assert_eq!(m.config().window_size, MAX_WINDOW);
        let m = HmmMatcher::new(MatcherConfig {
            window_size: 0,
            ..MatcherConfig::default()
        });
        assert_eq!(m.config().window_size, 1);
    }

    #[test]
    fn candidate_truncation_keeps_closest() {
        let mut slot = Slot::default();
        for i in 0..(MAX_CANDIDATES + 5) {
            let d = ((i * 7) % 13) as f64;
            HmmMatcher::insert_by_distance(
                &mut slot,
                Candidate {
                    proj: RouteProjection {
                        segment_index: i,
                        distance_from_route_m: d,
                        ..RouteProjection::default()
                    },
                    log_emission: 0.0,
                },
            );
        }
        assert_eq!(slot.len, MAX_CANDIDATES);
        for w in slot.candidates.windows(2) {
            assert!(w[0].proj.distance_from_route_m <= w[1].proj.distance_from_route_m);
        }
        // The 5 largest distances (of 0..13 pattern) must be gone.
        assert!(
            slot.candidates[MAX_CANDIDATES - 1]
                .proj
                .distance_from_route_m
                <= 8.0
        );
    }

    #[test]
    fn rejects_non_finite() {
        let index = out_and_back();
        let bad = MatchInput {
            position: Enu::new(f64::NAN, 0.0),
            sigma_m: 5.0,
            heading_rad: None,
            timestamp_ms: 0,
        };
        assert_eq!(
            HmmMatcher::new(MatcherConfig::default()).match_fix(&index, &bad),
            Err(NavError::InvalidLocation)
        );
        assert_eq!(
            SimpleMatcher.match_fix(&index, &bad),
            Err(NavError::InvalidLocation)
        );
    }
}
