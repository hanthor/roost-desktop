//! Prompt state machine: turns the daemon's auth conversation into UI
//! states with no password-specific flow. Every prompt shape renders
//! generically; failure collapses to one notice that never echoes
//! daemon detail (no user-vs-secret distinction leaks).

use greetd_ipc::{AuthMessageType, Response};

/// One auth question to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Message text shown to the user.
    pub text: String,
    /// Whether the answer must be masked during input.
    pub secret: bool,
}

/// What the model needs next after absorbing a daemon response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    /// Render `prompt` and collect an answer (`None` answers nothing).
    NeedAnswer { prompt: Prompt },
    /// Send `PostAuthMessageResponse { response: None }` unprompted
    /// (info/error messages carry no question).
    AutoAck,
    /// Authentication done; the client may start the session.
    Authenticated,
    /// Authentication failed; `notice` holds the generic text to show.
    Failed,
}

/// Which screen the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// Picking user and session, no conversation open.
    #[default]
    UserPick,
    /// Conversation in flight.
    Prompting,
    /// Terminal failure notice shown.
    Failed,
    /// Session start requested; waiting on handoff.
    Launching,
}

/// A session the user can launch (full enumeration lands later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub name: String,
    pub command: Vec<String>,
}

/// The single state the login UI renders from.
#[derive(Debug, Default)]
pub struct GreeterModel {
    pub user: Option<String>,
    pub session: Option<SessionRef>,
    pub pending: Vec<Prompt>,
    pub screen: Screen,
    pub notice: Option<String>,
}

impl GreeterModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a conversation for `user`, clearing prior state.
    pub fn begin_user(&mut self, user: &str) {
        self.user = Some(user.to_string());
        self.pending.clear();
        self.notice = None;
        self.screen = Screen::Prompting;
    }

    /// Return to user-pick, dropping conversation state (cancel path).
    pub fn cancel(&mut self) {
        self.user = None;
        self.pending.clear();
        self.notice = None;
        self.screen = Screen::UserPick;
    }

    /// Current prompt awaiting an answer, if any.
    pub fn current_prompt(&self) -> Option<&Prompt> {
        self.pending.last()
    }

    /// Absorb one daemon response; the caller acts on the event.
    pub fn apply_response(&mut self, response: &Response) -> ModelEvent {
        match response {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => match auth_message_type {
                AuthMessageType::Visible => {
                    let prompt = Prompt {
                        text: auth_message.clone(),
                        secret: false,
                    };
                    self.pending.push(prompt.clone());
                    ModelEvent::NeedAnswer { prompt }
                }
                AuthMessageType::Secret => {
                    let prompt = Prompt {
                        text: auth_message.clone(),
                        secret: true,
                    };
                    self.pending.push(prompt.clone());
                    ModelEvent::NeedAnswer { prompt }
                }
                AuthMessageType::Info | AuthMessageType::Error => {
                    self.notice = Some(auth_message.clone());
                    ModelEvent::AutoAck
                }
            },
            Response::Success => {
                self.pending.clear();
                self.notice = None;
                ModelEvent::Authenticated
            }
            Response::Error { .. } => {
                self.pending.clear();
                // Deliberately generic: never echo the daemon's
                // description, which could distinguish bad user from
                // bad secret or leak service detail.
                self.notice = Some("Sign-in failed. Try again.".to_string());
                self.screen = Screen::Failed;
                ModelEvent::Failed
            }
        }
    }

    /// Mark the session as starting (after the client sends StartSession).
    pub fn mark_session_starting(&mut self) {
        self.screen = Screen::Launching;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::ErrorType;

    fn secret_msg(text: &str) -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: text.to_string(),
        }
    }

    #[test]
    fn password_flow_needs_masked_answer() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        let event = model.apply_response(&secret_msg("Password:"));
        assert_eq!(
            event,
            ModelEvent::NeedAnswer {
                prompt: Prompt {
                    text: "Password:".to_string(),
                    secret: true,
                }
            }
        );
        assert_eq!(model.screen, Screen::Prompting);
    }

    #[test]
    fn multi_prompt_accumulates_in_order() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        model.apply_response(&secret_msg("Password:"));
        model.apply_response(&Response::AuthMessage {
            auth_message_type: AuthMessageType::Visible,
            auth_message: "Token:".to_string(),
        });
        assert_eq!(model.pending.len(), 2);
        assert_eq!(model.current_prompt().unwrap().text, "Token:");
        assert!(!model.current_prompt().unwrap().secret);
    }

    #[test]
    fn info_message_sets_notice_and_auto_acks() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        let event = model.apply_response(&Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: "Welcome.".to_string(),
        });
        assert_eq!(event, ModelEvent::AutoAck);
        assert_eq!(model.notice.as_deref(), Some("Welcome."));
        assert!(model.pending.is_empty());
    }

    #[test]
    fn auth_error_collapses_to_generic_notice() {
        let mut model = GreeterModel::new();
        model.begin_user("nosuchuser");
        let event = model.apply_response(&Response::Error {
            error_type: ErrorType::AuthError,
            description: "PAM authentication failed for nosuchuser".to_string(),
        });
        assert_eq!(event, ModelEvent::Failed);
        assert_eq!(model.screen, Screen::Failed);
        let notice = model.notice.unwrap();
        assert!(!notice.contains("nosuchuser"));
        assert!(!notice.contains("PAM"));
    }

    #[test]
    fn service_error_is_generic_too() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        let event = model.apply_response(&Response::Error {
            error_type: ErrorType::Error,
            description: "session socket vanished".to_string(),
        });
        assert_eq!(event, ModelEvent::Failed);
        assert!(!model.notice.unwrap().contains("socket"));
    }

    #[test]
    fn success_clears_prompts() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        model.apply_response(&secret_msg("Password:"));
        assert_eq!(
            model.apply_response(&Response::Success),
            ModelEvent::Authenticated
        );
        assert!(model.pending.is_empty());
        model.mark_session_starting();
        assert_eq!(model.screen, Screen::Launching);
    }

    #[test]
    fn cancel_returns_to_user_pick() {
        let mut model = GreeterModel::new();
        model.begin_user("ada");
        model.apply_response(&secret_msg("Password:"));
        model.cancel();
        assert_eq!(model.screen, Screen::UserPick);
        assert!(model.user.is_none());
        assert!(model.pending.is_empty());
        assert!(model.notice.is_none());
    }
}
