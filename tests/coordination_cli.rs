use std::{
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
    serde_json::from_slice(&output.stdout).expect("json output")
}

#[test]
fn lease_heartbeat_reclaim_and_handoff_are_atomic() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(workspace, &["add", "Lease task", "--json"]);
    let id = issue["id"].as_str().expect("issue id");

    let claimed = json(
        workspace,
        &[
            "claim",
            id,
            "--actor",
            "agent-a",
            "--lease-seconds",
            "30",
            "--json",
        ],
    );
    assert_eq!(claimed["status"], "in_progress");

    let heartbeat = json(
        workspace,
        &[
            "heartbeat",
            id,
            "--actor",
            "agent-a",
            "--lease-seconds",
            "30",
            "--json",
        ],
    );
    assert_eq!(heartbeat["owner"], "agent-a");

    let handoff = json(
        workspace,
        &[
            "handoff",
            id,
            "--from",
            "agent-a",
            "--to",
            "agent-b",
            "--summary",
            "storage work is ready",
            "--next",
            "run integration tests",
            "--json",
        ],
    );
    assert_eq!(handoff["to_actor"], "agent-b");

    let foreign = run(workspace, &["release", id, "--actor", "agent-b"]);
    assert!(!foreign.status.success());

    let not_expired = run(
        workspace,
        &["reclaim", id, "--supervisor", "supervisor", "--json"],
    );
    assert!(!not_expired.status.success());

    let released = json(workspace, &["release", id, "--actor", "agent-a", "--json"]);
    assert_eq!(released["status"], "open");

    let events = json(workspace, &["events", "--json"]);
    let events = events["events"].as_array().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event["operation"] == "handoff_created")
    );
}
