//! `navcore` — pure Rust turn-by-turn navigation core.
//!
//! This crate has no FFI and no knowledge of UniFFI. It is consumed by
//! `navcore-ffi`, which only converts types and forwards calls.
//!
//! Units: metres, seconds, degrees for lat/lng, radians internally for bearings.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod filter;
#[cfg(feature = "serde")]
pub mod fixtures;
pub mod geo;
pub mod types;

pub use types::{
    GeoPoint, ManeuverType, NavError, RawLocation, Route, RouteStep, SnappedLocation, TripProgress,
    TripState,
};
