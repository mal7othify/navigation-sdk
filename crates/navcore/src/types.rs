//! Public value types shared across the core.
//!
//! All distances are metres, all times are milliseconds since the Unix epoch
//! unless the field name says otherwise, and lat/lng are degrees.

use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A WGS-84 coordinate in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GeoPoint {
    /// Latitude in degrees, `[-90, 90]`.
    pub lat: f64,
    /// Longitude in degrees, `[-180, 180]`.
    pub lng: f64,
}

impl GeoPoint {
    /// Construct from degrees.
    pub const fn new(lat: f64, lng: f64) -> Self {
        Self { lat, lng }
    }

    /// True when both coordinates are finite and inside their valid ranges.
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite()
            && self.lng.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lng)
    }
}

/// A raw location fix as delivered by the host platform's location provider.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RawLocation {
    /// Reported position.
    pub point: GeoPoint,
    /// Horizontal accuracy in metres (1σ), if the provider reports one.
    pub accuracy_m: Option<f64>,
    /// Ground speed in metres per second, if reported.
    pub speed_mps: Option<f64>,
    /// Course over ground in degrees clockwise from true north, `[0, 360)`, if reported.
    pub course_deg: Option<f64>,
    /// Fix timestamp in milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
}

/// The kind of manoeuvre a [`RouteStep`] begins with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ManeuverType {
    /// Start of the route.
    Depart,
    /// Turn left.
    TurnLeft,
    /// Turn right.
    TurnRight,
    /// Bear left.
    SlightLeft,
    /// Bear right.
    SlightRight,
    /// Continue straight.
    Straight,
    /// Make a U-turn.
    UTurn,
    /// End of the route.
    Arrive,
}

/// One instruction-bearing leg of a [`Route`], covering geometry vertices
/// `start_index..=end_index`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RouteStep {
    /// Human-readable instruction, e.g. "Turn left onto High Street".
    pub instruction: String,
    /// The manoeuvre performed at the *start* of this step.
    pub maneuver: ManeuverType,
    /// Index into `Route::geometry` where this step starts.
    pub start_index: usize,
    /// Index into `Route::geometry` where this step ends (inclusive). The next
    /// step's `start_index` equals this value.
    pub end_index: usize,
    /// Length of this step along the geometry, metres.
    pub distance_m: f64,
}

/// A route: a polyline plus the steps that partition it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Route {
    /// Route polyline, at least two vertices.
    pub geometry: Vec<GeoPoint>,
    /// Steps in order; contiguous and covering the whole geometry.
    pub steps: Vec<RouteStep>,
}

impl Route {
    /// Structural validation: enough geometry, finite coordinates, contiguous
    /// steps that cover the whole polyline.
    pub fn validate(&self) -> Result<(), NavError> {
        if self.geometry.len() < 2 {
            return Err(NavError::InvalidRoute(
                "route geometry needs at least two vertices".into(),
            ));
        }
        if let Some(i) = self.geometry.iter().position(|p| !p.is_valid()) {
            return Err(NavError::InvalidRoute(format!(
                "geometry vertex {i} has an invalid coordinate"
            )));
        }
        if self.steps.is_empty() {
            return Err(NavError::InvalidRoute("route has no steps".into()));
        }
        let last_vertex = self.geometry.len() - 1;
        let mut expected_start = 0usize;
        for (i, step) in self.steps.iter().enumerate() {
            if step.start_index != expected_start {
                return Err(NavError::InvalidRoute(format!(
                    "step {i} starts at vertex {} but previous step ended at {expected_start}",
                    step.start_index
                )));
            }
            if step.end_index < step.start_index {
                return Err(NavError::InvalidRoute(format!(
                    "step {i} ends before it starts"
                )));
            }
            if step.end_index > last_vertex {
                return Err(NavError::InvalidRoute(format!(
                    "step {i} ends at vertex {} but geometry has {} vertices",
                    step.end_index,
                    self.geometry.len()
                )));
            }
            if !step.distance_m.is_finite() || step.distance_m < 0.0 {
                return Err(NavError::InvalidRoute(format!(
                    "step {i} has an invalid distance"
                )));
            }
            expected_start = step.end_index;
        }
        if expected_start != last_vertex {
            return Err(NavError::InvalidRoute(format!(
                "steps end at vertex {expected_start} but geometry ends at {last_vertex}"
            )));
        }
        Ok(())
    }

    /// Parse a route from JSON. Requires the `serde` feature.
    #[cfg(feature = "serde")]
    pub fn from_json(json: &str) -> Result<Self, NavError> {
        let route: Route =
            serde_json::from_str(json).map_err(|e| NavError::InvalidJson(e.to_string()))?;
        route.validate()?;
        Ok(route)
    }
}

/// A location snapped onto the route polyline.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SnappedLocation {
    /// Snapped position on the route.
    pub point: GeoPoint,
    /// Index of the segment (`geometry[i]..geometry[i+1]`) the point lies on.
    pub segment_index: usize,
    /// Distance from the route origin to the snapped point, metres.
    pub distance_along_route_m: f64,
    /// Perpendicular distance from the raw/filtered position to the route, metres.
    pub distance_from_route_m: f64,
    /// Bearing of the route segment at the snapped point, degrees clockwise from north, `[0, 360)`.
    pub bearing_deg: f64,
}

/// Coarse trip lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TripProgress {
    /// No location has been accepted yet.
    NotStarted,
    /// Actively navigating.
    Navigating,
    /// Arrived at the final vertex of the last step.
    Arrived,
}

/// Everything the host UI needs after each accepted location update.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TripState {
    /// Snapped position.
    pub snapped: SnappedLocation,
    /// Index into `Route::steps` of the step currently being driven.
    pub current_step_index: usize,
    /// Distance along the route to the end of the current step, metres.
    pub distance_to_next_maneuver_m: f64,
    /// Distance along the route from the snapped position to the destination, metres.
    pub distance_remaining_m: f64,
    /// Instruction for the upcoming manoeuvre (the next step's instruction).
    pub next_instruction: String,
    /// True after enough consecutive fixes beyond the off-route distance.
    pub is_off_route: bool,
    /// True when the core wants the host to fetch a new route and call `set_route`.
    pub needs_reroute: bool,
    /// Coarse lifecycle.
    pub progress: TripProgress,
}

/// All errors the core can produce. Mirrored 1:1 by the UniFFI error enum.
#[derive(Debug, Clone, PartialEq)]
pub enum NavError {
    /// The route failed structural validation.
    InvalidRoute(String),
    /// The fix is not newer than the last accepted fix.
    StaleLocation {
        /// Timestamp of the rejected fix.
        timestamp_ms: u64,
        /// Timestamp of the last accepted fix.
        last_timestamp_ms: u64,
    },
    /// The fix implies a speed above `NavigatorConfig::max_speed_mps`.
    ImplausibleLocation {
        /// Speed implied by the fix relative to the last accepted one, m/s.
        implied_speed_mps: f64,
        /// Configured limit, m/s.
        max_speed_mps: f64,
    },
    /// The fix has a non-finite or out-of-range coordinate.
    InvalidLocation,
    /// JSON could not be parsed into a route.
    InvalidJson(String),
}

impl fmt::Display for NavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NavError::InvalidRoute(reason) => write!(f, "invalid route: {reason}"),
            NavError::StaleLocation {
                timestamp_ms,
                last_timestamp_ms,
            } => write!(
                f,
                "stale location: {timestamp_ms} ms is not after {last_timestamp_ms} ms"
            ),
            NavError::ImplausibleLocation {
                implied_speed_mps,
                max_speed_mps,
            } => write!(
                f,
                "implausible location: implied speed {implied_speed_mps:.1} m/s exceeds {max_speed_mps:.1} m/s"
            ),
            NavError::InvalidLocation => write!(f, "invalid location coordinate"),
            NavError::InvalidJson(reason) => write!(f, "invalid json: {reason}"),
        }
    }
}

impl std::error::Error for NavError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(start: usize, end: usize) -> RouteStep {
        RouteStep {
            instruction: String::new(),
            maneuver: ManeuverType::Straight,
            start_index: start,
            end_index: end,
            distance_m: 1.0,
        }
    }

    fn geometry(n: usize) -> Vec<GeoPoint> {
        (0..n)
            .map(|i| GeoPoint::new(0.0, i as f64 * 0.001))
            .collect()
    }

    #[test]
    fn valid_route_passes() {
        let route = Route {
            geometry: geometry(5),
            steps: vec![step(0, 2), step(2, 4)],
        };
        assert_eq!(route.validate(), Ok(()));
    }

    #[test]
    fn rejects_short_geometry() {
        let route = Route {
            geometry: geometry(1),
            steps: vec![step(0, 0)],
        };
        assert!(matches!(route.validate(), Err(NavError::InvalidRoute(_))));
    }

    #[test]
    fn rejects_gap_between_steps() {
        let route = Route {
            geometry: geometry(5),
            steps: vec![step(0, 2), step(3, 4)],
        };
        assert!(route.validate().is_err());
    }

    #[test]
    fn rejects_steps_not_reaching_end() {
        let route = Route {
            geometry: geometry(5),
            steps: vec![step(0, 3)],
        };
        assert!(route.validate().is_err());
    }

    #[test]
    fn rejects_invalid_coordinate() {
        let mut g = geometry(3);
        g[1].lat = 95.0;
        let route = Route {
            geometry: g,
            steps: vec![step(0, 2)],
        };
        assert!(route.validate().is_err());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn json_roundtrip() {
        let route = Route {
            geometry: geometry(3),
            steps: vec![step(0, 2)],
        };
        let json = serde_json::to_string(&route).unwrap();
        let back = Route::from_json(&json).unwrap();
        assert_eq!(back, route);
    }
}
