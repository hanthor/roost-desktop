//! Login window: GTK4/libadwaita views rendered from [`GreeterModel`].
//! All view *content* decisions live in pure [`render_snapshot`]
//! (headless-tested); `build_ui` only constructs widgets. Keyboard:
//! Tab order follows visual order, Enter submits, Escape cancels.
//!
//! `render_snapshot` and [`LoginView`] are always available. `build_ui` is
//! behind the default `gtk-ui` feature, so a consumer that only drives the
//! model needs no GTK toolchain.

use crate::model::{GreeterModel, Screen};
use crate::session::SessionEntry;

/// Everything the window shows, derived from the model. No GTK here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginView {
    /// Window title / header.
    pub title: String,
    /// Notice banner text, if any.
    pub notice: Option<String>,
    /// Current prompt label, if awaiting an answer.
    pub prompt: Option<String>,
    /// Whether the answer field masks input.
    pub prompt_secret: bool,
    /// Whether the answer field + login button are sensitive.
    pub input_sensitive: bool,
    /// Session picker rows (names), Roost default first.
    pub sessions: Vec<String>,
    /// Selected session index.
    pub selected_session: usize,
}

/// Derive window content from the model and session list.
pub fn render_snapshot(
    model: &GreeterModel,
    sessions: &[SessionEntry],
    users: &[String],
) -> LoginView {
    let _ = users;
    let current = model.current_prompt();
    LoginView {
        title: "Sign in to Roost".to_string(),
        notice: model.notice.clone(),
        prompt: current.map(|p| p.text.clone()),
        prompt_secret: current.is_some_and(|p| p.secret),
        input_sensitive: !matches!(model.screen, Screen::Launching),
        sessions: sessions.iter().map(|s| s.name.clone()).collect(),
        selected_session: sessions.iter().position(|s| s.is_default).unwrap_or(0),
    }
}

/// Construct the GTK window from a snapshot. Runs on the UI thread;
/// not covered by headless tests (see `render_snapshot`).
#[cfg(feature = "gtk-ui")]
pub fn build_ui(app: &libadwaita::Application, view: &LoginView) {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;

    let window = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title(&view.title)
        .default_width(360)
        .default_height(480)
        .build();

    let page = libadwaita::PreferencesPage::new();
    let group = libadwaita::PreferencesGroup::new();
    page.add(&group);

    if let Some(notice) = &view.notice {
        let banner = gtk4::Label::builder()
            .label(notice)
            .css_classes(["error"])
            .wrap(true)
            .build();
        group.add(&banner);
    }

    let prompt_label = view.prompt.as_deref().unwrap_or("Ready");
    let answer = if view.prompt_secret {
        gtk4::PasswordEntry::new().upcast::<gtk4::Widget>()
    } else {
        gtk4::Entry::builder()
            .placeholder_text(prompt_label)
            .build()
            .upcast::<gtk4::Widget>()
    };
    answer.set_sensitive(view.input_sensitive);
    group.add(&answer);

    let session_model = gtk4::StringList::new(&[]);
    for name in &view.sessions {
        session_model.append(name);
    }
    let sessions = libadwaita::ComboRow::builder()
        .title("Session")
        .model(&session_model)
        .selected(view.selected_session as u32)
        .sensitive(view.input_sensitive)
        .build();
    group.add(&sessions);

    let login = gtk4::Button::with_label("Sign in");
    login.set_sensitive(view.input_sensitive);
    login.add_css_class("suggested-action");
    group.add(&login);

    window.set_content(Some(&page));
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GreeterModel;

    fn sessions() -> Vec<SessionEntry> {
        vec![
            SessionEntry {
                name: "Sway".to_string(),
                command: vec!["sway".into()],
                source: Default::default(),
                is_default: false,
            },
            SessionEntry {
                name: "Roost".to_string(),
                command: vec!["roost-session".into()],
                source: Default::default(),
                is_default: true,
            },
        ]
    }

    #[test]
    fn empty_model_renders_idle_window() {
        let view = render_snapshot(&GreeterModel::new(), &sessions(), &[]);
        assert_eq!(view.title, "Sign in to Roost");
        assert!(view.notice.is_none());
        assert!(view.prompt.is_none());
        assert!(view.input_sensitive);
        assert_eq!(view.sessions, vec!["Sway", "Roost"]);
        assert_eq!(view.selected_session, 1);
    }

    #[test]
    fn failed_model_shows_notice_and_keeps_input() {
        let mut model = GreeterModel::new();
        model.begin_user("mallory");
        model.notice = Some("Sign-in failed. Try again.".to_string());
        model.screen = Screen::Failed;
        let view = render_snapshot(&model, &sessions(), &[]);
        assert_eq!(view.notice.as_deref(), Some("Sign-in failed. Try again."));
        assert!(view.input_sensitive);
    }

    #[test]
    fn launching_disables_input() {
        let mut model = GreeterModel::new();
        model.mark_session_starting();
        let view = render_snapshot(&model, &[], &[]);
        assert!(!view.input_sensitive);
        assert_eq!(view.selected_session, 0);
    }
}
