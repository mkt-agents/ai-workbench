use super::*;

#[test]
fn interrupted_effect_survives_reopen_and_must_be_reviewed() {
    let dir = temp_workspace("reopen");
    let db = dir.0.join("tasks.db");
    let workspace = dir.0.join("work");
    std::fs::create_dir(&workspace).unwrap();
    let scope = workspace.to_string_lossy().into_owned();
    let conn = Connection::open(&db).unwrap();
    init(&conn).unwrap();
    start(
        &conn,
        &TaskRequest::new("s", "t"),
        "r1",
        "Fix the example",
        &scope,
        &[],
    )
    .unwrap();
    assert!(start(
        &conn,
        &TaskRequest::new("s", "other"),
        "r2",
        "other",
        &scope,
        &[]
    )
    .is_err());
    let completed = begin_action(&conn, "t", "r1", "write_file", "first", &[], false).unwrap();
    finish_action(&conn, "t", "r1", completed, Some(true), None, None).unwrap();
    begin_action(&conn, "t", "r1", "write_file", "second", &[], false).unwrap();
    drop(conn);
    let conn = Connection::open(&db).unwrap();
    init(&conn).unwrap();
    recover(&conn).unwrap();
    let view = load(&conn, "s").unwrap().unwrap();
    assert_eq!(view.reason.as_deref(), Some("app_interrupted"));
    assert_eq!(
        view.actions
            .iter()
            .map(|a| a.status.as_str())
            .collect::<Vec<_>>(),
        ["done", "uncertain"]
    );
    let resumed = start(&conn, &TaskRequest::resume("s", "t"), "r3", "", &scope, &[]).unwrap();
    assert!(resumed.needs_review);
    assert!(begin_action(&conn, "t", "r3", "write_file", "guarded", &[], false).is_err());
    approve_review(&conn, "t", "r3").unwrap();
    begin_action(&conn, "t", "r3", "write_file", "fresh", &[], false).unwrap();
    assert_eq!(load(&conn, "s").unwrap().unwrap().actions.len(), 3);
}

#[test]
fn verification_uses_exit_status_and_is_invalidated_by_an_edit() {
    let conn = Connection::open_in_memory().unwrap();
    init(&conn).unwrap();
    start(&conn, &TaskRequest::new("s", "t"), "r", "goal", "", &[]).unwrap();
    for (code, expected) in [(1, "failed"), (0, "passed")] {
        let id = begin_action(&conn, "t", "r", "run_command", "check", &[], true).unwrap();
        finish_action(&conn, "t", "r", id, Some(code == 0), Some(code), None).unwrap();
        assert_eq!(
            load(&conn, "s").unwrap().unwrap().verification.status,
            expected
        );
    }
    begin_action(&conn, "t", "r", "edit_file", "edit", &[], false).unwrap();
    assert_eq!(
        load(&conn, "s").unwrap().unwrap().verification.status,
        "stale"
    );
}

#[test]
fn failed_intent_transaction_preserves_previous_evidence() {
    let conn = Connection::open_in_memory().unwrap();
    init(&conn).unwrap();
    start(&conn, &TaskRequest::new("s", "t"), "r", "goal", "", &[]).unwrap();
    let id = begin_action(&conn, "t", "r", "run_command", "check", &[], true).unwrap();
    finish_action(&conn, "t", "r", id, Some(true), Some(0), None).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_action BEFORE INSERT ON agent_task_actions BEGIN SELECT RAISE(ABORT, 'disk failure'); END;").unwrap();
    assert!(begin_action(&conn, "t", "r", "write_file", "edit", &[], false).is_err());
    assert_eq!(
        load(&conn, "s").unwrap().unwrap().verification.status,
        "passed"
    );
}

struct TempWorkspace(std::path::PathBuf);
impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let target = self.0.canonicalize();
        let temp = std::env::temp_dir().canonicalize();
        if let (Ok(target), Ok(temp)) = (target, temp) {
            if target.parent() == Some(temp.as_path())
                && target
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("aw-agent-task-"))
            {
                let _ = std::fs::remove_dir_all(target);
            }
        }
    }
}

fn temp_workspace(label: &str) -> TempWorkspace {
    let path = std::env::temp_dir().join(format!(
        "aw-agent-task-{label}-{}-{}",
        std::process::id(),
        now()
    ));
    std::fs::create_dir(&path).unwrap();
    TempWorkspace(path)
}

#[test]
fn changed_branch_requires_review_even_at_the_same_commit() {
    let dir = temp_workspace("branch");
    let root = dir.0.to_string_lossy();
    for args in [
        &["init", "-b", "original"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ][..],
    ] {
        crate::git_commands::git_stdout(&root, args).unwrap();
    }
    let conn = Connection::open_in_memory().unwrap();
    init(&conn).unwrap();
    start(&conn, &TaskRequest::new("s", "t"), "r1", "goal", &root, &[]).unwrap();
    conn.execute(
        "UPDATE agent_tasks SET status = 'waiting_approval' WHERE task_id = 't'",
        [],
    )
    .unwrap();
    recover(&conn).unwrap();
    crate::git_commands::git_stdout(&root, &["switch", "-c", "same-commit"]).unwrap();
    let resumed = start(&conn, &TaskRequest::resume("s", "t"), "r2", "", "", &[]).unwrap();
    assert_eq!(resumed.branch.as_deref(), Some("same-commit"));
    assert!(resumed.needs_review);
    assert!(resumed.actions.is_empty());
    assert!(start(&conn, &TaskRequest::resume("s", "t"), "r3", "", "", &[]).is_err());
    std::fs::write(dir.0.join("elsewhere.txt"), "changed since review started").unwrap();
    assert!(approve_review(&conn, "t", "r2")
        .unwrap_err()
        .to_string()
        .contains("TASK_WORKSPACE_CHANGED_DURING_REVIEW"));
    assert!(begin_action(&conn, "t", "r2", "write_file", "guarded", &[], false).is_err());
}

#[test]
fn a_write_ignored_by_git_still_invalidates_a_recorded_path() {
    let dir = temp_workspace("paths");
    let root = dir.0.to_string_lossy();
    crate::git_commands::git_stdout(&root, &["init"]).unwrap();
    std::fs::write(dir.0.join(".gitignore"), "ignored.txt\n").unwrap();
    let file = dir.0.join("ignored.txt");
    std::fs::write(&file, b"old").unwrap();
    let paths = vec![file.to_string_lossy().into_owned()];
    let conn = Connection::open_in_memory().unwrap();
    init(&conn).unwrap();
    start(&conn, &TaskRequest::new("s", "t"), "r1", "goal", &root, &[]).unwrap();
    let action = begin_action(&conn, "t", "r1", "write_file", "file", &paths, false).unwrap();
    finish_action(&conn, "t", "r1", action, Some(true), None, None).unwrap();
    let action = begin_action(&conn, "t", "r1", "run_command", "check", &[], true).unwrap();
    finish_action(&conn, "t", "r1", action, Some(true), Some(0), None).unwrap();
    let mut evidence = current_verification(&conn, "t").unwrap();
    std::fs::write(&file, b"new").unwrap();
    let current = WorkspaceState::capture(&root, &[], paths);
    stale_if_changed(&mut evidence, &current);
    assert_eq!(evidence.status, "stale");
}

#[test]
fn failed_result_transaction_leaves_the_action_uncertain_after_recovery() {
    let conn = Connection::open_in_memory().unwrap();
    init(&conn).unwrap();
    start(&conn, &TaskRequest::new("s", "t"), "r1", "goal", "", &[]).unwrap();
    let action = begin_action(&conn, "t", "r1", "write_file", "file", &[], false).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_result BEFORE UPDATE ON agent_task_actions BEGIN SELECT RAISE(ABORT, 'disk failure'); END;").unwrap();
    assert!(finish_action(&conn, "t", "r1", action, Some(true), None, None).is_err());
    conn.execute_batch("DROP TRIGGER fail_result;").unwrap();
    recover(&conn).unwrap();
    let previous = load(&conn, "s").unwrap().unwrap();
    assert!(previous.needs_review);
    assert_eq!(previous.actions[0].status, "uncertain");
}
