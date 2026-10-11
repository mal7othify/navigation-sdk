//! Synthetic routes anchored at a real position, for testing the navigator on
//! foot without a map or a routing service.
//!
//! [`walk_course`] lays out a short zigzag starting at the device's current
//! position so a tester can literally walk it: straight, a right turn, a left
//! turn, arrive. The destination is deliberately well away from the start:
//! on a closed loop the first fix is a coin toss between "at the start" and
//! "at the end", and the losing side of that toss is an instant arrival.
//! [`direct_route`] is the "reroute" a host can install when the tester
//! deliberately leaves the course: a straight line from where they are to
//! the destination.
//!
//! Geometry is built in a local ENU frame around the origin and converted
//! back to WGS-84, with a vertex every [`VERTEX_SPACING_M`] so the matcher
//! and any schematic drawing have short, regular segments.

use crate::geo::{haversine_m, Enu, LocalFrame};
use crate::types::{GeoPoint, ManeuverType, NavError, Route, RouteStep};

/// Spacing of intermediate vertices along each leg, metres.
pub const VERTEX_SPACING_M: f64 = 10.0;
/// Shortest leg that is still meaningfully longer than phone GPS noise.
pub const MIN_LEG_M: f64 = 10.0;
/// Longest leg accepted (keeps the course walkable).
pub const MAX_LEG_M: f64 = 2_000.0;
/// Longest [`direct_route`] accepted. A straight line is only a meaningful
/// stand-in reroute locally, and with a vertex every 10 m an unbounded one
/// (say, from a tester's garden to a fixture route on another continent)
/// would be hundreds of thousands of points.
pub const MAX_DIRECT_ROUTE_M: f64 = 50_000.0;

/// A zigzag walking course of three legs of `leg_m`, starting at `origin`
/// heading `heading_deg` (degrees clockwise from north): straight, turn
/// right, turn left, arrive. Four steps: `Depart`, `TurnRight`, `TurnLeft`,
/// `Arrive`. The destination ends up `√5 · leg_m` from the start.
pub fn walk_course(origin: GeoPoint, heading_deg: f64, leg_m: f64) -> Result<Route, NavError> {
    if !origin.is_valid() {
        return Err(NavError::InvalidLocation);
    }
    if !heading_deg.is_finite() {
        return Err(NavError::InvalidRoute("heading must be finite".into()));
    }
    if !leg_m.is_finite() || !(MIN_LEG_M..=MAX_LEG_M).contains(&leg_m) {
        return Err(NavError::InvalidRoute(format!(
            "leg length {leg_m} m must be between {MIN_LEG_M} and {MAX_LEG_M} m"
        )));
    }

    let frame = LocalFrame::new(origin);
    let mut builder = Builder::new(Enu::new(0.0, 0.0));
    let mut bearing = heading_deg.to_radians();
    builder.leg(
        bearing,
        leg_m,
        ManeuverType::Depart,
        format!("Walk straight for {} m", leg_m.round()),
    );
    bearing += std::f64::consts::FRAC_PI_2;
    builder.leg(
        bearing,
        leg_m,
        ManeuverType::TurnRight,
        format!("Turn right, then {} m", leg_m.round()),
    );
    bearing -= std::f64::consts::FRAC_PI_2;
    builder.leg(
        bearing,
        leg_m,
        ManeuverType::TurnLeft,
        format!("Turn left, then {} m", leg_m.round()),
    );
    builder.finish(&frame, "Arrive")
}

/// A straight route from `from` to `to`: `Depart` then `Arrive`. Hosts use it
/// as the stand-in reroute when the navigator asks for one.
pub fn direct_route(from: GeoPoint, to: GeoPoint) -> Result<Route, NavError> {
    if !from.is_valid() || !to.is_valid() {
        return Err(NavError::InvalidLocation);
    }
    let length = haversine_m(from, to);
    if length < 1.0 {
        return Err(NavError::InvalidRoute(
            "direct route needs two distinct points".into(),
        ));
    }
    if length > MAX_DIRECT_ROUTE_M {
        return Err(NavError::InvalidRoute(format!(
            "direct route of {} km exceeds the {} km limit",
            (length / 1000.0).round(),
            MAX_DIRECT_ROUTE_M / 1000.0
        )));
    }
    let frame = LocalFrame::new(from);
    let target = frame.to_enu(to);
    let bearing = target.x.atan2(target.y);
    let mut builder = Builder::new(Enu::new(0.0, 0.0));
    builder.leg(
        bearing,
        length,
        ManeuverType::Depart,
        format!("Head to the destination, {} m", length.round()),
    );
    builder.finish(&frame, "Arrive")
}

/// Accumulates legs as ENU vertices and steps that follow the OSRM convention
/// (manoeuvre at the start of a step, zero-length `Arrive` last).
struct Builder {
    vertices: Vec<Enu>,
    steps: Vec<RouteStep>,
}

impl Builder {
    fn new(start: Enu) -> Self {
        Self {
            vertices: vec![start],
            steps: Vec::new(),
        }
    }

    /// Append a straight leg of `length_m` on `bearing_rad` (clockwise from
    /// north), subdivided every [`VERTEX_SPACING_M`].
    fn leg(
        &mut self,
        bearing_rad: f64,
        length_m: f64,
        maneuver: ManeuverType,
        instruction: String,
    ) {
        let start_index = self.vertices.len() - 1;
        let from = self.vertices[start_index];
        let (dx, dy) = (bearing_rad.sin(), bearing_rad.cos());
        let n = (length_m / VERTEX_SPACING_M).ceil().max(1.0) as usize;
        for i in 1..=n {
            let d = (i as f64 * VERTEX_SPACING_M).min(length_m);
            self.vertices
                .push(Enu::new(from.x + dx * d, from.y + dy * d));
        }
        self.steps.push(RouteStep {
            instruction,
            maneuver,
            start_index,
            end_index: self.vertices.len() - 1,
            distance_m: length_m,
        });
    }

    fn finish(mut self, frame: &LocalFrame, arrive_instruction: &str) -> Result<Route, NavError> {
        let last = self.vertices.len() - 1;
        self.steps.push(RouteStep {
            instruction: arrive_instruction.to_string(),
            maneuver: ManeuverType::Arrive,
            start_index: last,
            end_index: last,
            distance_m: 0.0,
        });
        let route = Route {
            geometry: self.vertices.iter().map(|e| frame.to_geo(*e)).collect(),
            steps: self.steps,
        };
        route.validate()?;
        Ok(route)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::RouteIndex;
    use crate::navigator::{Navigator, NavigatorConfig};
    use crate::types::{RawLocation, TripProgress};

    const RIYADH: GeoPoint = GeoPoint::new(24.7136, 46.6753);

    #[test]
    fn walk_course_is_an_open_zigzag() {
        let route = walk_course(RIYADH, 30.0, 50.0).unwrap();
        assert_eq!(route.steps.len(), 4);
        assert_eq!(route.steps[0].maneuver, ManeuverType::Depart);
        assert_eq!(route.steps[1].maneuver, ManeuverType::TurnRight);
        assert_eq!(route.steps[2].maneuver, ManeuverType::TurnLeft);
        assert_eq!(route.steps[3].maneuver, ManeuverType::Arrive);
        let index = RouteIndex::new(&route.geometry).unwrap();
        assert!((index.total_length_m() - 150.0).abs() < 0.5);
        // The destination is well clear of the start: √5 legs away.
        let end = *route.geometry.last().unwrap();
        assert!((haversine_m(RIYADH, end) - 50.0 * 5f64.sqrt()).abs() < 0.5);
        // Vertices every 10 m: 3 legs × 5 + the origin.
        assert_eq!(route.geometry.len(), 16);
    }

    #[test]
    fn walk_course_rejects_bad_input() {
        assert!(matches!(
            walk_course(RIYADH, 0.0, 5.0),
            Err(NavError::InvalidRoute(_))
        ));
        assert!(matches!(
            walk_course(RIYADH, f64::NAN, 50.0),
            Err(NavError::InvalidRoute(_))
        ));
        assert!(matches!(
            walk_course(GeoPoint::new(91.0, 0.0), 0.0, 50.0),
            Err(NavError::InvalidLocation)
        ));
    }

    #[test]
    fn direct_route_matches_great_circle_distance() {
        let to = GeoPoint::new(24.7150, 46.6760);
        let route = direct_route(RIYADH, to).unwrap();
        assert_eq!(route.steps.len(), 2);
        let index = RouteIndex::new(&route.geometry).unwrap();
        let expected = haversine_m(RIYADH, to);
        assert!((index.total_length_m() - expected).abs() < 0.5);
        assert!(haversine_m(*route.geometry.last().unwrap(), to) < 0.5);
        assert!(matches!(
            direct_route(RIYADH, RIYADH),
            Err(NavError::InvalidRoute(_))
        ));
        // Riyadh → Berlin is not a reroute.
        assert!(matches!(
            direct_route(RIYADH, GeoPoint::new(52.52, 13.405)),
            Err(NavError::InvalidRoute(_))
        ));
    }

    /// A pedestrian who actually walks the course must see every step and
    /// arrive, with the default configuration.
    #[test]
    fn walking_the_course_advances_every_step_and_arrives() {
        let heading = 30.0_f64;
        let leg = 50.0;
        let route = walk_course(RIYADH, heading, leg).unwrap();
        let frame = LocalFrame::new(RIYADH);
        let mut nav = Navigator::new(route, NavigatorConfig::default()).unwrap();

        // 1.4 m/s along each leg, one fix per second, exact positions.
        let mut pos = Enu::new(0.0, 0.0);
        let turns = [
            0.0,
            std::f64::consts::FRAC_PI_2,
            -std::f64::consts::FRAC_PI_2,
        ];
        let mut bearing = heading.to_radians();
        let mut t: u64 = 0;
        let mut seen = Vec::new();
        let mut last = None;
        for turn in turns {
            bearing += turn;
            let (dx, dy) = (bearing.sin(), bearing.cos());
            let mut walked = 0.0;
            while walked < leg {
                let step = (leg - walked).min(1.4);
                walked += step;
                pos = Enu::new(pos.x + dx * step, pos.y + dy * step);
                let s = nav
                    .update_location(RawLocation {
                        point: frame.to_geo(pos),
                        accuracy_m: Some(4.0),
                        speed_mps: Some(1.4),
                        course_deg: Some(bearing.to_degrees().rem_euclid(360.0)),
                        timestamp_ms: 1_700_000_000_000 + t * 1000,
                    })
                    .unwrap();
                t += 1;
                if seen.last() != Some(&s.trip.current_step_index) {
                    seen.push(s.trip.current_step_index);
                }
                assert!(!s.position.is_off_route, "fix {t} off route");
                last = Some(s);
            }
        }
        assert_eq!(seen, vec![0, 1, 2, 3]);
        let last = last.unwrap();
        assert_eq!(last.position.progress, TripProgress::Arrived);
        assert!((last.position.speed_mps - 1.4).abs() < 0.5);
    }
}
