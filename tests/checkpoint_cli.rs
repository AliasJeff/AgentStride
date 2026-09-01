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
fn checkpoint_and_context_survive_new_commands() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();

    assert!(run(workspace, &["init"]).status.success());
    let issue = json(workspace, &["add", "Implement recovery", "--json"]);
    let issue_id = issue["id"].as_str().expect("issue id");

    let claimed = json(
        workspace,
        &["claim", issue_id, "--actor", "agent-a", "--json"],
    );
    assert_eq!(claimed["status"], "in_progress");

    let checkpoint = json(
        workspace,
        &[
            "checkpoint",
            issue_id,
            "--actor",
            "agent-a",
            "--summary",
            "storage is ready",
            "--completed",
            "added checkpoint table",
            "--next",
            "wire prime",
            "--evidence",
            "cargo test",
            "--json",
        ],
    );
    assert_eq!(checkpoint["revision"], 1);

    let context = json(workspace, &["context", issue_id, "--json"]);
    assert_eq!(context["issue"]["id"], issue_id);
    assert_eq!(context["checkpoint"]["next_step"], "wire prime");

    let events = json(workspace, &["events", "--json"]);
    assert!(
        events["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| event["operation"] == "checkpoint_updated")
    );
}

#[test]
fn checkpoint_rejects_a_foreign_actor() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();

    assert!(run(workspace, &["init"]).status.success());
    let issue = json(workspace, &["add", "Owned task", "--json"]);
    let issue_id = issue["id"].as_str().expect("issue id");
    assert!(
        run(
            workspace,
            &["claim", issue_id, "--actor", "agent-a", "--json"]
        )
        .status
        .success()
    );

    let output = run(
        workspace,
        &[
            "checkpoint",
            issue_id,
            "--actor",
            "agent-b",
            "--summary",
            "must fail",
            "--next",
            "none",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not owned"));
}
