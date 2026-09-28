//! Contract tests: full auth exchanges against the scripted fake
//! daemon. Success, bad password, multi-prompt, and session crash.

mod fake_greetd;

use std::os::unix::net::UnixStream;
use std::thread;

use fake_greetd::{auth_error, run_script, secret_prompt, visible_prompt, Step};
use greetd_ipc::Response;
use rwd_greeter::{
    client::GreeterClient,
    model::{GreeterModel, ModelEvent},
};

fn pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().unwrap()
}

#[test]
fn success_exchange_signs_in() {
    let (a, b) = pair();
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

    let mut client = GreeterClient::from_stream(b);
    let mut model = GreeterModel::new();
    model.begin_user("ada");

    let event = model.apply_response(&client.create_session("ada").unwrap());
    assert!(matches!(event, ModelEvent::NeedAnswer { .. }));
    let event = model.apply_response(&client.answer(Some("s3cret".into())).unwrap());
    assert_eq!(event, ModelEvent::Authenticated);
    model.mark_session_starting();
    client
        .start_session(vec!["rwd-session".into()], vec![])
        .unwrap();
    assert_eq!(model.screen, rwd_greeter::model::Screen::Launching);
    handle.join().unwrap();
}

#[test]
fn bad_password_stays_generic() {
    let (a, b) = pair();
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
                    reply: Some(auth_error()),
                },
            ],
        )
    });

    let mut client = GreeterClient::from_stream(b);
    let mut model = GreeterModel::new();
    model.begin_user("mallory");
    let _ = model.apply_response(&client.create_session("mallory").unwrap());
    let event = model.apply_response(&client.answer(Some("wrong".into())).unwrap());
    assert_eq!(event, ModelEvent::Failed);
    let notice = model.notice.unwrap();
    assert!(!notice.contains("mallory"));
    handle.join().unwrap();
}

#[test]
fn multi_prompt_password_plus_token() {
    let (a, b) = pair();
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
                    reply: Some(visible_prompt("Token:")),
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

    let mut client = GreeterClient::from_stream(b);
    let mut model = GreeterModel::new();
    model.begin_user("ada");
    let e1 = model.apply_response(&client.create_session("ada").unwrap());
    assert!(matches!(e1, ModelEvent::NeedAnswer { .. }));
    let e2 = model.apply_response(&client.answer(Some("s3cret".into())).unwrap());
    assert!(matches!(e2, ModelEvent::NeedAnswer { .. }));
    assert_eq!(model.pending.len(), 2);
    let e3 = model.apply_response(&client.answer(Some("123456".into())).unwrap());
    assert_eq!(e3, ModelEvent::Authenticated);
    client
        .start_session(vec!["rwd-session".into()], vec![])
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn session_crash_after_start_is_observable() {
    let (a, b) = pair();
    let handle = thread::spawn(move || {
        run_script(
            a,
            vec![
                Step::Exchange {
                    expect_kind: "create",
                    reply: Some(Response::Success),
                },
                Step::CrashAfter {
                    expect_kind: "start",
                },
            ],
        )
    });

    let probe = b.try_clone().unwrap();
    let mut client = GreeterClient::from_stream(b);
    let mut model = GreeterModel::new();
    model.begin_user("ada");
    let event = model.apply_response(&client.create_session("ada").unwrap());
    assert_eq!(event, ModelEvent::Authenticated);
    client
        .start_session(vec!["rwd-session".into()], vec![])
        .unwrap();
    model.mark_session_starting();
    // The dead session shows up as a broken conversation: with the
    // peer gone, the next wire read fails cleanly (EOF) instead of
    // hanging or yielding a phantom message.
    use greetd_ipc::codec::SyncCodec;
    let mut raw = probe;
    assert!(greetd_ipc::Response::read_from(&mut raw).is_err());
    drop(client);
    handle.join().unwrap();
}
