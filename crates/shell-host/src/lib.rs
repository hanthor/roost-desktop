//! Supervised shell host: Activities trigger and window list as a
//! separate Wayland client (001 spec R3/R5, ADR 0003).
//!
//! The binary entry point is `src/main.rs`. This library exposes the two
//! halves a later control-protocol adapter will join up:
//!
//! - [`model`] — plain shell-side view state (no dependency on any other
//!   workspace crate).
//! - [`panel`] — Wayland client setup for the top-anchored Activities
//!   panel surface.

pub mod model;
pub mod panel;
