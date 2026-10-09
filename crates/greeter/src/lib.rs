//! Tuna Desktop login greeter: prompt state machine, session enumeration, and
//! the greetd JSON-IPC client. The GTK login window
//! renders from [`model::GreeterModel`]; no password-specific flow
//! exists anywhere in this crate.

pub use tuna_greeter_control::{client, model};
pub use tuna_greeter_control::{GreeterClient, GreeterModel};

pub mod session;
pub mod ui;
