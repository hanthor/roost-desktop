// Greeter entry point: builds the login window from the model.
// Daemon conversation wiring lands with the walkthrough task.
use gtk4::gio::prelude::*;
use tuna_greeter::{model::GreeterModel, session::enumerate_system, ui};

/// Release version stamped at build time: `TUNA_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("TUNA_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        println!("tuna-greeter {}", release_version());
        return;
    }
    let app = libadwaita::Application::builder()
        .application_id("asia.reilly.tuna.greeter")
        .build();
    app.connect_activate(|app| {
        let model = GreeterModel::new();
        let sessions = enumerate_system();
        let users: Vec<String> = Vec::new();
        let view = ui::render_snapshot(&model, &sessions.entries, &users);
        ui::build_ui(app, &view);
    });
    app.run();
}

#[cfg(test)]
mod version_tests {
    use super::normalize_version;

    #[test]
    fn tag_prefix_is_stripped() {
        assert_eq!(normalize_version(Some("v1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn plain_version_is_kept() {
        assert_eq!(normalize_version(Some("1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn missing_or_empty_falls_back() {
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
    }
}
