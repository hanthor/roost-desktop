//! Shared greeter IPC protocol and prompt state machine types.
//!
//! Provides the greetd JSON-IPC client and prompt model used by both
//! `tuna-greeter` and `tuna-compositor` (for unlock flows), without
//! pulling in UI dependencies.

pub mod client;
pub mod model;

pub use client::GreeterClient;
pub use model::{GreeterModel, ModelEvent, Prompt, Screen, SessionRef};
