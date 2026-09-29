// Greeter entry point: builds the login window from the model.
// Daemon conversation wiring lands with the walkthrough task.
use gtk4::gio::prelude::*;
use roost_greeter::{model::GreeterModel, session::enumerate_system, ui};

fn main() {
    let app = libadwaita::Application::builder()
        .application_id("asia.reilly.roost.greeter")
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
