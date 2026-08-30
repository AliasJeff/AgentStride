use std::fs;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::tempdir;

fn run(project: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project)
        .args(args)
        .output()
        .expect("command should run")
}

fn json_command(project: &std::path::Path, args: &[&str]) -> Value {
    let output = run(project, args);
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command should emit JSON")
}

fn init(project: &std::path::Path) {
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project)
        .args(["init", "--quiet"])
        .assert()
        .success();
}

#[test]
fn event_replay_is_idempotent_and_preserves_exported_state() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let stream = source.path().join("events.jsonl");

    init(source.path());
    let created = json_command(
        source.path(),
        &["add", "Replay me", "--priority", "1", "--json"],
    );
    let id = created["id"].as_str().expect("issue id").to_owned();
    json_command(
        source.path(),
        &["update", &id, "--title", "Replayed", "--json"],
    );
    json_command(
        source.path(),
        &["claim", &id, "--actor", "worker-a", "--json"],
    );
    json_command(source.path(), &["close", &id, "--reason", "done", "--json"]);

    let events = run(source.path(), &["events"]);
    assert!(events.status.success());
    fs::write(&stream, events.stdout).expect("write event stream");

    let source_export = run(source.path(), &["export"]);
    assert!(source_export.status.success());

    let replay = run(
        source.path(),
        &[
            "replay",
            stream.to_str().expect("stream path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert!(replay.status.success());
    let result: Value = serde_json::from_slice(&replay.stdout).expect("replay result");
    assert_eq!(result["applied"], 4);
    assert_eq!(result["skipped"], 0);

    let replay_again = run(
        source.path(),
        &[
            "replay",
            stream.to_str().expect("stream path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert!(replay_again.status.success());
    let result: Value = serde_json::from_slice(&replay_again.stdout).expect("replay result");
    assert_eq!(result["applied"], 0);
    assert_eq!(result["skipped"], 4);

    let recovered_export = run(recovered.path(), &["export"]);
    assert!(recovered_export.status.success());
    assert_eq!(source_export.stdout, recovered_export.stdout);

    let verified = json_command(recovered.path(), &["verify", "--json"]);
    assert_eq!(verified["valid"], true);

    let verified_by_path = json_command(
        source.path(),
        &["verify", recovered.path().to_str().expect("path"), "--json"],
    );
    assert_eq!(verified_by_path["valid"], true);
}

#[test]
fn replay_rejects_same_event_id_with_different_content() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let stream = source.path().join("events.jsonl");
    let conflict = source.path().join("conflict.jsonl");
    init(source.path());
    json_command(source.path(), &["add", "Event identity", "--json"]);

    let events = run(source.path(), &["events"]);
    assert!(events.status.success());
    fs::write(&stream, &events.stdout).expect("write event stream");

    let first_replay = run(
        source.path(),
        &[
            "replay",
            stream.to_str().expect("stream path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert!(first_replay.status.success());

    let mut event: Value =
        serde_json::from_slice(&events.stdout).expect("single event from stream");
    event["payload"] = json!({ "changed": true });
    fs::write(&conflict, format!("{event}\n")).expect("write conflict stream");

    let second_replay = run(
        source.path(),
        &[
            "replay",
            conflict.to_str().expect("stream path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert_eq!(second_replay.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&second_replay.stderr);
    assert!(
        stderr.contains("event_id already exists"),
        "stderr: {stderr}"
    );
}

#[test]
fn export_uses_explicit_jsonl_kinds() {
    let project = tempdir().expect("workspace");
    init(project.path());
    json_command(project.path(), &["add", "Snapshot", "--json"]);

    let output = run(project.path(), &["export"]);
    assert!(output.status.success());
    let lines = String::from_utf8(output.stdout)
        .expect("utf8 export")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("jsonl record"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["record_kind"], "issue");
}

#[test]
fn snapshot_import_is_line_order_independent() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let snapshot = source.path().join("snapshot.jsonl");
    init(source.path());

    let blocker = json_command(source.path(), &["add", "Blocker", "--json"])["id"]
        .as_str()
        .expect("blocker id")
        .to_owned();
    let dependent = json_command(source.path(), &["add", "Dependent", "--json"])["id"]
        .as_str()
        .expect("dependent id")
        .to_owned();
    json_command(
        source.path(),
        &["link", &dependent, "--blocks", &blocker, "--json"],
    );
    let exported = run(source.path(), &["export"]);
    assert!(exported.status.success());
    let mut lines = String::from_utf8(exported.stdout)
        .expect("utf8 export")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.reverse();
    fs::write(&snapshot, format!("{}\n", lines.join("\n"))).expect("write snapshot");

    let imported = run(
        source.path(),
        &[
            "import",
            snapshot.to_str().expect("snapshot path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert!(imported.status.success());
    let result: Value = serde_json::from_slice(&imported.stdout).expect("import result");
    assert_eq!(result["issues"], 2);
    assert_eq!(result["edges"], 1);

    let source_export = run(source.path(), &["export"]);
    let recovered_export = run(recovered.path(), &["export"]);
    assert_eq!(source_export.stdout, recovered_export.stdout);
    let verified = json_command(recovered.path(), &["verify", "--json"]);
    assert_eq!(verified["valid"], true);
}

#[test]
fn snapshot_import_rolls_back_when_a_later_edge_is_invalid() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let snapshot = source.path().join("invalid-snapshot.jsonl");
    init(source.path());

    let imported_issue = json!({
        "record_kind": "issue",
        "id": "ag-imported",
        "title": "Imported",
        "description": "",
        "kind": "task",
        "status": "open",
        "priority": 1,
        "assignee": null,
        "created_at": "2026-09-26T00:00:00Z",
        "updated_at": "2026-09-26T00:00:00Z",
        "defer_until": null,
        "close_reason": "",
        "revision": 1
    });
    let invalid_edge = json!({
        "record_kind": "edge",
        "source_id": "ag-imported",
        "target_id": "ag-missing",
        "edge_type": "blocks",
        "created_at": "2026-09-26T00:00:00Z"
    });
    fs::write(&snapshot, format!("{imported_issue}\n{invalid_edge}\n"))
        .expect("write invalid snapshot");

    let output = run(
        source.path(),
        &[
            "import",
            snapshot.to_str().expect("snapshot path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let imported = json_command(recovered.path(), &["list", "--json"]);
    assert!(imported.as_array().expect("issue list").is_empty());
}

#[test]
fn malformed_replay_reports_line_number() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let input = source.path().join("bad.jsonl");
    init(source.path());
    fs::write(&input, b"{}\nnot-json\n").expect("write malformed stream");

    let output = run(
        source.path(),
        &[
            "replay",
            input.to_str().expect("input path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("line 1"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn missing_predecessor_and_unknown_operation_fail_loudly() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let input = source.path().join("invalid.jsonl");
    init(source.path());

    let issue = json!({
        "seq": 1,
        "event_id": "evt-missing-predecessor",
        "operation": "issue_created",
        "issue_id": "ag-abcdef",
        "actor": null,
        "created_at": "2026-09-26T00:00:00Z",
        "snapshot": {
            "id": "ag-abcdef",
            "title": "Missing",
            "description": "",
            "kind": "task",
            "status": "open",
            "priority": 1,
            "assignee": null,
            "created_at": "2026-09-26T00:00:00Z",
            "updated_at": "2026-09-26T00:00:00Z",
            "defer_until": null,
            "close_reason": "",
            "revision": 1
        },
        "payload": {}
    });
    let unknown = json!({
        "seq": 2,
        "event_id": "evt-unknown",
        "operation": "issue_deleted",
        "issue_id": null,
        "actor": null,
        "created_at": "2026-09-26T00:00:00Z",
        "snapshot": null,
        "payload": {}
    });
    fs::write(&input, format!("{}\n{}\n", issue, unknown)).expect("write invalid stream");

    let output = run(
        source.path(),
        &[
            "replay",
            input.to_str().expect("input path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("line 2"), "stderr: {stderr}");
    assert!(stderr.contains("unknown variant"), "stderr: {stderr}");
}

#[test]
fn missing_predecessor_reports_event_identity() {
    let source = tempdir().expect("source workspace");
    let recovered = tempdir().expect("recovered workspace");
    let input = source.path().join("missing.jsonl");
    init(source.path());

    let event = json!({
        "seq": 1,
        "event_id": "evt-missing-predecessor",
        "operation": "issue_updated",
        "issue_id": "ag-abcdef",
        "actor": null,
        "created_at": "2026-09-26T00:00:00Z",
        "snapshot": {
            "id": "ag-abcdef",
            "title": "Missing",
            "description": "",
            "kind": "task",
            "status": "open",
            "priority": 1,
            "assignee": null,
            "created_at": "2026-09-26T00:00:00Z",
            "updated_at": "2026-09-26T00:00:00Z",
            "defer_until": null,
            "close_reason": "",
            "revision": 2
        },
        "payload": {}
    });
    fs::write(&input, format!("{event}\n")).expect("write missing predecessor");

    let output = run(
        source.path(),
        &[
            "replay",
            input.to_str().expect("input path"),
            "--into",
            recovered.path().to_str().expect("recovery path"),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("line 1"), "stderr: {stderr}");
    assert!(
        stderr.contains("evt-missing-predecessor"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("predecessor"), "stderr: {stderr}");
}
