//! End-to-end walkthrough (headless): enumerate fixture sessions,
// pick Roost, run the full auth conversation against the scripted fake
//! daemon, and hand off to session start. The GTK window is excluded
//! by design (needs a display); it renders from the same model proven
//! here. Cold boot and real PAM stay manual per docs/greeter-vm-acceptance.md.

mod fake_greetd;

use std::os::unix::net::UnixStream;
use std::thread;

use fake_greetd::{run_script, secret_prompt, Step};
use greetd_ipc::Response;
use roost_greeter::{
    client::GreeterClient,
    model::{GreeterModel, ModelEvent},
    session::enumerate_dirs,
    ui::render_snapshot,
};

#[test]
fn full_sign_in_walkthrough() {
    // 1. Sessions enumerate from fixtures with Roost default.
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let listing = enumerate_dirs(&[&dir]);
    assert!(!listing.entries.is_empty());
    let chosen = listing
        .entries
        .iter()
        .find(|s| s.is_default)
        .unwrap()
        .clone();

    // 2. Fake daemon: password prompt, then success, then start.
    let (a, b) = UnixStream::pair().unwrap();
    let handle = thread::spawn(move || {
        run_script(
            a,
            vec![
                Step::Exchange {
                    expect_kind: "create",
                    reply: Some(secret_prompt("Password:")),
                },
                Step::Exchange {
                    expect_kind: "answer",
                    reply: Some(Response::Success),
                },
                Step::Exchange {
                    expect_kind: "start",
                    reply: None,
                },
            ],
        )
    });

    // 3. Drive the whole conversation through model + client.
    let mut client = GreeterClient::from_stream(b);
    let mut model = GreeterModel::new();
    model.begin_user("ada");
    model.session = Some(roost_greeter::model::SessionRef {
        name: chosen.name.clone(),
        command: chosen.command.clone(),
    });
    assert!(matches!(
        model.apply_response(&client.create_session("ada").unwrap()),
        ModelEvent::NeedAnswer { .. }
    ));
    assert_eq!(
        model.apply_response(&client.answer(Some("s3cret".into())).unwrap()),
        ModelEvent::Authenticated
    );
    client
        .start_session(chosen.command.clone(), vec![])
        .unwrap();
    model.mark_session_starting();

    // 4. The window content derives from the proven model state.
    let view = render_snapshot(&model, &listing.entries, &["ada".to_string()]);
    assert!(!view.input_sensitive);
    assert!(view.prompt.is_none());
    handle.join().unwrap();
}
