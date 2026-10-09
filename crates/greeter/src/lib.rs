//! Tuna Desktop login greeter: prompt state machine, session enumeration, and
//! the greetd JSON-IPC client. The GTK login window (later task)
//! renders from [`model::GreeterModel`]; no password-specific flow
//! exists anywhere in this crate.

pub mod client;
pub mod model;
pub mod session;
pub mod ui;
