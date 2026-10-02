use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_as"))
        .args(["--workspace", workspace.to_str().expect("workspace")])
        .args(args)
        .output()
        .expect("run as")
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
fn prime_contains_claim_checkpoint_ready_and_bounded_memory() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    fs::write(
        workspace.join(".agent-stride/PRIME.md"),
        "repository rule: run the focused test first",
    )
    .expect("prime instructions");

    let current = json(workspace, &["add", "Current task", "--json"]);
    let current_id = current["id"].as_str().expect("current id");
    let ready = json(workspace, &["add", "Ready task", "--json"]);
    let ready_id = ready["id"].as_str().expect("ready id");
    assert!(
        run(
            workspace,
            &["claim", current_id, "--actor", "agent-a", "--json"]
        )
        .status
        .success()
    );
    assert!(
        run(
            workspace,
            &[
                "checkpoint",
                current_id,
                "--actor",
                "agent-a",
                "--summary",
                "started",
                "--next",
                "continue",
                "--json",
            ]
        )
        .status
        .success()
    );
    assert!(
        run(
            workspace,
            &[
                "remember",
                "SQLite writes use transactions",
                "--key",
                "transaction-rule",
                "--json",
            ]
        )
        .status
        .success()
    );
    assert!(
        run(
            workspace,
            &[
                "remember",
                "This second memory should be capped",
                "--key",
                "second-rule",
                "--json",
            ]
        )
        .status
        .success()
    );

    let context = json(
        workspace,
        &[
            "prime",
            "--actor",
            "agent-a",
            "--max-memory-count",
            "1",
            "--json",
        ],
    );
    assert_eq!(context["schema_version"], 1);
    assert_eq!(context["current_issue"]["id"], current_id);
    assert_eq!(context["checkpoint"]["next_step"], "continue");
    assert_eq!(context["memories"].as_array().expect("memories").len(), 1);
    assert!(context["truncated"].as_bool().expect("truncated"));
    assert!(
        context["ready"]
            .as_array()
            .expect("ready")
            .iter()
            .any(|issue| issue["id"] == ready_id)
    );
    assert_eq!(
        context["custom_instructions"],
        "repository rule: run the focused test first"
    );
}
