//! JSON fixture format shared by tests, benchmarks and the demo apps.
//! Only available with the `serde` feature.
//!
//! ```json
//! {
//!   "name": "route_simple",
//!   "route": { "geometry": [...], "steps": [...] },
//!   "trace": [ { "point": {...}, "accuracy_m": 8.0, "speed_mps": 12.1, "course_deg": 90.0, "timestamp_ms": 0 }, ... ],
//!   "truth": [ { "lat": ..., "lng": ... }, ... ],
//!   "expected": { ... }
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::types::{GeoPoint, NavError, RawLocation, Route};

/// Assertions a full simulated drive over the fixture must satisfy.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Expected {
    /// `current_step_index` values in the order they should first appear.
    #[serde(default)]
    pub step_sequence: Vec<usize>,
    /// Trace indices (inclusive ranges) during which the trip should be off-route.
    #[serde(default)]
    pub off_route_ranges: Vec<[usize; 2]>,
    /// Whether the drive should end in `TripProgress::Arrived`.
    #[serde(default)]
    pub arrives: bool,
}

/// A route, a simulated raw GPS trace over it, and optional ground truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
    /// Short identifier.
    pub name: String,
    /// The route being driven.
    pub route: Route,
    /// Raw fixes in time order.
    pub trace: Vec<RawLocation>,
    /// True position for each fix (same length as `trace`), if known.
    #[serde(default)]
    pub truth: Vec<GeoPoint>,
    /// Drive-level assertions.
    #[serde(default)]
    pub expected: Expected,
}

impl Fixture {
    /// Parse and validate.
    pub fn from_json(json: &str) -> Result<Self, NavError> {
        let f: Fixture =
            serde_json::from_str(json).map_err(|e| NavError::InvalidJson(e.to_string()))?;
        f.route.validate()?;
        if !f.truth.is_empty() && f.truth.len() != f.trace.len() {
            return Err(NavError::InvalidJson(format!(
                "truth has {} entries but trace has {}",
                f.truth.len(),
                f.trace.len()
            )));
        }
        Ok(f)
    }

    /// Read from disk.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, NavError> {
        let json =
            std::fs::read_to_string(path).map_err(|e| NavError::InvalidJson(e.to_string()))?;
        Self::from_json(&json)
    }

    /// Serialise (pretty-printed) for writing fixtures.
    pub fn to_json(&self) -> String {
        // Serialising our own plain data cannot fail.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Path to the repository `fixtures/` directory, for tests.
pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
