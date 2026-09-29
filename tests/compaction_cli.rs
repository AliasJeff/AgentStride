use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ag"))
        .args(["--workspace", workspace.to_str().expect("workspace")])
        .args(args)
        .output()
        .expect("run ag")
}

fn json(workspace: &Path, args: &[&str]) -> Value {
    let output = run(workspace, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

#[test]
fn compact_then_restore_is_reversible_and_journaled() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(
        workspace,
        &[
            "add",
            "Long task",
            "--description",
            "original design and verification details",
            "--json",
        ],
    );
    let id = issue["id"].as_str().expect("id");

    let compacted = json(
        workspace,
        &[
            "compact",
            id,
            "--summary",
            "summary: design complete; tests pending",
            "--json",
        ],
    );
    assert_eq!(
        compacted["description"],
        "summary: design complete; tests pending"
    );

    let restored = json(workspace, &["restore", id, "--json"]);
    assert_eq!(
        restored["description"],
        "original design and verification details"
    );

    let second_restore = run(workspace, &["restore", id, "--json"]);
    assert!(!second_restore.status.success());
    let events = json(workspace, &["events", "--json"]);
    let events = events["events"].as_array().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event["operation"] == "issue_compacted")
    );
    assert!(
        events
            .iter()
            .any(|event| event["operation"] == "issue_restored")
    );

    let replay_input = project.path().join("events.jsonl");
    let jsonl = events
        .iter()
        .map(|event| serde_json::to_string(event).expect("event json"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&replay_input, format!("{jsonl}\n")).expect("write replay");
    let recovered = TempDir::new().expect("recovered");
    let replay = run(
        workspace,
        &[
            "--json",
            "replay",
            replay_input.to_str().expect("input"),
            "--into",
            recovered.path().to_str().expect("recovered"),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let recovered_issue = json(recovered.path(), &["show", id, "--json"]);
    assert_eq!(
        recovered_issue["description"],
        "original design and verification details"
    );
}

#[test]
fn analyze_lists_closed_candidates_without_mutating() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(
        workspace,
        &[
            "add",
            "Old task",
            "--description",
            "a sufficiently long task description",
            "--json",
        ],
    );
    let id = issue["id"].as_str().expect("id").to_owned();
    assert!(
        run(workspace, &["close", &id, "--reason", "done", "--json"],)
            .status
            .success()
    );

    let analyzed = json(workspace, &["compact", "--analyze", "--json"]);
    assert_eq!(analyzed["kind"], "analyze");
    assert_eq!(analyzed["candidates"][0]["issue"]["id"], id);
    assert_eq!(
        json(workspace, &["show", &id, "--json"])["description"],
        "a sufficiently long task description"
    );
}

#[test]
fn summary_must_shrink_content_and_failed_apply_keeps_original() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(
        workspace,
        &[
            "add",
            "Task",
            "--description",
            "original description",
            "--json",
        ],
    );
    let id = issue["id"].as_str().expect("id");

    let failed = run(
        workspace,
        &[
            "compact",
            id,
            "--summary",
            "this summary is longer than the original description",
            "--json",
        ],
    );
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("shorter than original"));
    assert_eq!(
        json(workspace, &["show", id, "--json"])["description"],
        "original description"
    );
}

#[test]
fn auto_uses_external_json_summarizer_and_records_metadata() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(
        workspace,
        &[
            "add",
            "Old task",
            "--description",
            "original description with implementation and verification details",
            "--json",
        ],
    );
    let id = issue["id"].as_str().expect("id").to_owned();
    assert!(
        run(workspace, &["close", &id, "--reason", "done", "--json"],)
            .status
            .success()
    );

    let result = json(
        workspace,
        &[
            "compact",
            "--auto",
            &id,
            "--command",
            "sh",
            "--command-arg=-c",
            "--command-arg",
            "cat >/dev/null; printf '{\"summary\":\"implementation verified\"}'",
            "--actor",
            "agent-a",
            "--model",
            "test-model",
            "--json",
        ],
    );
    assert_eq!(result["kind"], "auto");
    assert_eq!(result["results"][0]["status"], "compacted");
    assert_eq!(
        json(workspace, &["show", &id, "--json"])["description"],
        "implementation verified"
    );

    let events = json(workspace, &["events", "--json"]);
    let event = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["operation"] == "issue_compacted")
        .expect("compaction event");
    assert_eq!(event["payload"]["metadata"]["actor"], "agent-a");
    assert_eq!(event["payload"]["metadata"]["model"], "test-model");
    assert_eq!(event["payload"]["metadata"]["original_bytes"], 65);
    assert_eq!(event["payload"]["metadata"]["compacted_bytes"], 23);
}

#[test]
fn auto_timeout_keeps_original_content() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(
        workspace,
        &[
            "add",
            "Slow task",
            "--description",
            "original implementation details",
            "--json",
        ],
    );
    let id = issue["id"].as_str().expect("id").to_owned();
    assert!(
        run(workspace, &["close", &id, "--reason", "done", "--json"])
            .status
            .success()
    );

    let result = json(
        workspace,
        &[
            "compact",
            "--auto",
            &id,
            "--command",
            "sh",
            "--command-arg=-c",
            "--command-arg",
            "sleep 1; printf '{\"summary\":\"too late\"}'",
            "--timeout-seconds",
            "0",
            "--retries",
            "0",
            "--json",
        ],
    );
    assert_eq!(result["results"][0]["status"], "skipped");
    assert!(
        result["results"][0]["error"]
            .as_str()
            .expect("timeout error")
            .contains("timed out")
    );
    assert_eq!(
        json(workspace, &["show", &id, "--json"])["description"],
        "original implementation details"
    );
}
