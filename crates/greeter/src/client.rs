//! Greetd JSON-IPC client: blocking conversation driver over a Unix
//! stream using the adopted `greetd_ipc` crate's sync codec. One
//! method per protocol step; the caller (later: the UI task) drives
//! the prompt loop against [`crate::model::GreeterModel`].

use std::os::unix::net::UnixStream;

use greetd_ipc::{
    codec::{Error as CodecError, SyncCodec},
    Request, Response,
};

/// A connected conversation with the login daemon.
pub struct GreeterClient {
    stream: UnixStream,
}

impl GreeterClient {
    /// Connect to the daemon at `socket_path` (`$GREETD_SOCK` live).
    pub fn connect(socket_path: &str) -> std::io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(socket_path)?,
        })
    }

    /// Wrap an already-connected stream (tests, harnesses).
    pub fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Open a login attempt for `user`; returns the daemon's first
    /// response (auth message, success, or failure).
    pub fn create_session(&mut self, user: &str) -> Result<Response, CodecError> {
        Request::CreateSession {
            username: user.to_string(),
        }
        .write_to(&mut self.stream)?;
        Response::read_from(&mut self.stream)
    }

    /// Answer the last auth message (`None` acknowledges info/error
    /// messages that carry no question).
    pub fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        Request::PostAuthMessageResponse { response: text }.write_to(&mut self.stream)?;
        Response::read_from(&mut self.stream)
    }

    /// Start the authenticated session. The daemon execs on success,
    /// so no response follows — this returns once the request is sent.
    pub fn start_session(&mut self, cmd: Vec<String>, env: Vec<String>) -> Result<(), CodecError> {
        Request::StartSession { cmd, env }.write_to(&mut self.stream)
    }

    /// Abort before start. Unnecessary after an error (the daemon
    /// auto-cancels), but harmless; kept for the UI cancel path.
    pub fn cancel(&mut self) -> Result<(), CodecError> {
        Request::CancelSession.write_to(&mut self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::AuthMessageType;
    use std::thread;

    /// Scripted peer: answers CreateSession with one secret prompt.
    fn peer(mut stream: UnixStream) {
        let req = Request::read_from(&mut stream).unwrap();
        assert!(matches!(req, Request::CreateSession { .. }));
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".to_string(),
        }
        .write_to(&mut stream)
        .unwrap();
    }

    #[test]
    fn create_session_roundtrip() {
        let (a, b) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || peer(a));
        let mut client = GreeterClient::from_stream(b);
        let resp = client.create_session("ada").unwrap();
        assert!(matches!(
            resp,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                ..
            }
        ));
        handle.join().unwrap();
    }
}
