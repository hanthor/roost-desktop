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
//! - [`control`] — shell-side control-protocol client (Hello, snapshot,
//!   ordered changes with gap resnapshot, token activation commands).

pub mod apps;
pub mod control;
pub mod dock;
pub mod extensions;
pub mod favorites;
pub mod ibus;
pub mod icons;
pub mod intake;
pub mod introspect;
pub mod keyboard;
pub mod model;
pub mod notifications;
pub mod overview;
pub mod panel;
pub mod popup;
pub mod prefs;
pub mod search;
pub mod settings;
pub mod tiles;
pub mod watcher;
pub mod xdg;
