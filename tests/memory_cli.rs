use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn command(project: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = Command::cargo_bin("as").expect("binary");
    command.current_dir(project.path()).args(args);
    command.assert()
}

fn json_command(project: &TempDir, args: &[&str]) -> Value {
    let output = command(project, args).success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("json output")
}

#[test]
fn memories_persist_and_can_be_filtered() {
    let project = tempfile::tempdir().expect("project");
    command(&project, &["init", "--quiet"]).success();

    let created = json_command(
        &project,
        &[
            "remember",
            "SQLite writes use transactions",
            "--scope",
            "workspace",
            "--json",
        ],
    );
    assert_eq!(created["revision"], 1);
    assert_eq!(created["scope"], "workspace");

    let listed = json_command(&project, &["memories", "--search", "SQLITE", "--json"]);
    assert_eq!(listed[0]["key"], "sqlite-writes-use-transactions");

    let recalled = json_command(
        &project,
        &["recall", "sqlite-writes-use-transactions", "--json"],
    );
    assert_eq!(recalled["content"], "SQLite writes use transactions");
}

#[test]
fn memory_update_and_forget_are_visible_in_journal() {
    let project = tempfile::tempdir().expect("project");
    command(&project, &["init", "--quiet"]).success();
    command(&project, &["remember", "--key", "rule", "first", "--json"]).success();
    command(&project, &["remember", "--key", "rule", "second", "--json"]).success();

    command(&project, &["forget", "rule", "--json"])
        .success()
        .stdout(predicate::str::contains("deleted"));

    let events = json_command(&project, &["events", "--since", "0", "--json"]);
    let operations: Vec<&str> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event["operation"].as_str().expect("operation"))
        .collect();
    assert_eq!(
        operations,
        vec!["memory_created", "memory_updated", "memory_deleted"]
    );
}

#[test]
fn memory_governance_hides_expired_and_can_archive_it() {
    let project = tempfile::tempdir().expect("project");
    command(&project, &["init", "--quiet"]).success();
    let created = json_command(
        &project,
        &[
            "remember",
            "temporary fact",
            "--key",
            "temporary",
            "--expires-at",
            "2020-01-01T00:00:00Z",
            "--importance",
            "5",
            "--confidence",
            "4",
            "--json",
        ],
    );
    assert_eq!(created["importance"], 5);
    assert_eq!(created["confidence"], 4);

    let visible = json_command(&project, &["memories", "--json"]);
    assert!(visible.as_array().expect("visible memories").is_empty());

    command(&project, &["govern-memories", "--archive-stale", "--json"]).success();
    let archived = json_command(&project, &["memories", "--include-archived", "--json"]);
    assert_eq!(archived[0]["status"], "archived");
}

#[test]
fn archived_memory_event_can_be_replayed() {
    let project = tempfile::tempdir().expect("project");
    command(&project, &["init", "--quiet"]).success();
    command(
        &project,
        &[
            "remember",
            "temporary fact",
            "--key",
            "temporary",
            "--expires-at",
            "2020-01-01T00:00:00Z",
            "--json",
        ],
    )
    .success();
    command(&project, &["govern-memories", "--archive-stale", "--json"]).success();

    let events = json_command(&project, &["events", "--since", "0", "--json"]);
    let archive_event = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["payload"]["governance"] == "archive_stale")
        .expect("archive event");
    assert_eq!(archive_event["snapshot"]["status"], "archived");
    assert_eq!(archive_event["snapshot"]["revision"], 2);
}
