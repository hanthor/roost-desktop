//! Scripted fake greetd peer for contract tests: speaks the real
//! `greetd_ipc` wire over a socketpair from a canned script. Never
//! touches PAM or a display.

// Shared across integration targets that each use a subset.
#![allow(dead_code)]

use std::os::unix::net::UnixStream;

use greetd_ipc::{codec::SyncCodec, Request, Response};

/// One scripted step: what to expect, what to reply.
pub enum Step {
    /// Expect this request (checked by debug shape), reply with this.
    Exchange {
        expect_kind: &'static str,
        reply: Option<Response>,
    },
    /// Expect this request, then drop the connection (crash).
    CrashAfter { expect_kind: &'static str },
}

fn kind_of(req: &Request) -> &'static str {
    match req {
        Request::CreateSession { .. } => "create",
        Request::PostAuthMessageResponse { .. } => "answer",
        Request::StartSession { .. } => "start",
        Request::CancelSession => "cancel",
    }
}

/// Run `script` against `stream`, returning the requests seen.
pub fn run_script(mut stream: UnixStream, script: Vec<Step>) -> Vec<String> {
    let mut seen = Vec::new();
    for step in script {
        let req = Request::read_from(&mut stream).expect("fake peer read");
        let kind = kind_of(&req);
        seen.push(kind.to_string());
        match step {
            Step::Exchange { expect_kind, reply } => {
                assert_eq!(kind, expect_kind, "protocol order violated");
                if let Some(resp) = reply {
                    resp.write_to(&mut stream).expect("fake peer write");
                }
            }
            Step::CrashAfter { expect_kind } => {
                assert_eq!(kind, expect_kind, "protocol order violated");
                return seen;
            }
        }
    }
    seen
}

pub fn secret_prompt(text: &str) -> Response {
    Response::AuthMessage {
        auth_message_type: greetd_ipc::AuthMessageType::Secret,
        auth_message: text.to_string(),
    }
}

pub fn visible_prompt(text: &str) -> Response {
    Response::AuthMessage {
        auth_message_type: greetd_ipc::AuthMessageType::Visible,
        auth_message: text.to_string(),
    }
}

pub fn auth_error() -> Response {
    Response::Error {
        error_type: greetd_ipc::ErrorType::AuthError,
        description: "PAM authentication failed".to_string(),
    }
}
