//! `navcore-ffi` — UniFFI layer over [`navcore`].
//!
//! This crate only converts types and forwards calls. No logic lives here.
//! No panics may cross the FFI boundary: every fallible operation returns
//! `Result<_, NavError>`.
