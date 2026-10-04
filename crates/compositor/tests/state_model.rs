//! Public-API round trip for the revisioned state model: insert, snapshot,
//! incremental sync, and gap-triggered resnapshot.

use roost_compositor::state::{DenyReason, StateModel, TokenDecision, TokenPolicy, WindowUpdate};

#[test]
fn snapshot_sync_then_incremental_then_gap_resnapshot() {
    let mut m = StateModel::new();
    let a = m.insert("term", None, 1);
    let b = m.insert("browser", None, 2);
    assert!(m.set_focused(Some(b)));

    // Snapshot-on-connect carries everything.
    let snap = m.snapshot();
    assert_eq!(snap.revision, m.revision());
    assert_eq!(snap.windows.len(), 2);
    assert_eq!(snap.workspaces, vec![0, 1, 2]);
    assert_eq!(snap.active, 0);
    assert_eq!(snap.focused, Some(b));

    // Incremental catch-up from the snapshot revision.
    let rev = snap.revision;
    assert!(m.update(
        a,
        WindowUpdate {
            title: Some("term!".into()),
            ..Default::default()
        }
    ));
    let tail = m.changes_since(rev).unwrap();
    assert_eq!(tail.len(), 1);

    // Unknown future revision signals resnapshot, not an empty tail.
    let gap = m.changes_since(rev + 100).unwrap_err();
    assert_eq!(gap.got, rev + 100);
    assert_eq!(gap.expected, m.revision());
}

#[test]
fn token_policy_end_to_end() {
    assert_eq!(
        TokenPolicy::validate(0, 1_000, TokenPolicy::MAX_AGE_MS, true, true),
        TokenDecision::Allow
    );
    assert_eq!(
        TokenPolicy::validate(0, 1_000, TokenPolicy::MAX_AGE_MS, true, false),
        TokenDecision::Deny {
            reason: DenyReason::AppMismatch
        }
    );
}

#[test]
fn workspace_insertion_shifts_windows_and_active_workspace() {
    let mut m = StateModel::new();
    let dragged = m.insert("drag", None, 0);
    let home = m.insert("home", None, 0);
    let right = m.insert("right", None, 1);
    m.set_active_workspace(1);
    assert!(m.insert_workspace_and_move(dragged, 1));
    assert_eq!(m.window(dragged).unwrap().workspace, 1);
    assert_eq!(m.window(home).unwrap().workspace, 0);
    assert_eq!(m.window(right).unwrap().workspace, 2);
    assert_eq!(m.active_workspace(), 2);
    assert_eq!(m.workspaces(), &[0, 1, 2]);
    let before = m.snapshot();
    assert!(!m.insert_workspace_and_move(u64::MAX, 0));
    assert_eq!(m.snapshot(), before);
    m.remove(dragged);
    assert_eq!(m.workspaces(), &[0, 2]);
}
