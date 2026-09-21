use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

use agent_stride::{
    domain::Actor,
    runtime::{Command as RuntimeCommand, CommandKind, RunId, TaskId},
    store::Store,
};

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
fn runner_requires_a_valid_token_and_checks_policy() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());
    let issue = json(workspace, &["add", "Runner task", "--json"]);
    let id = issue["id"].as_str().expect("issue id");
    assert!(
        run(
            workspace,
            &[
                "claim",
                id,
                "--actor",
                "agent-a",
                "--lease-seconds",
                "30",
                "--json",
            ]
        )
        .status
        .success()
    );

    let token = json(workspace, &["--json", "token", id, "--actor", "agent-a"])["token"]
        .as_str()
        .expect("token")
        .to_owned();

    let result = json(
        workspace,
        &[
            "--json",
            "run",
            id,
            "--actor",
            "agent-a",
            "--token",
            &token,
            "cargo",
            "--version",
        ],
    );
    assert!(result["success"].as_bool().expect("success"));
    assert!(result["stdout"].as_str().expect("stdout").contains("cargo"));

    let mut tampered = token.clone();
    tampered.push('0');
    let invalid = run(
        workspace,
        &[
            "--json",
            "run",
            id,
            "--actor",
            "agent-a",
            "--token",
            &tampered,
            "cargo",
            "--version",
        ],
    );
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("token"));

    let denied = run(
        workspace,
        &[
            "--json", "run", id, "--actor", "agent-a", "--token", &token, "sh", "-c", "true",
        ],
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("not allowed"));
}

#[test]
fn worker_handoff_cli_updates_state_and_routes_the_next_attempt() {
    let project = TempDir::new().expect("project");
    let workspace = project.path();
    assert!(run(workspace, &["init"]).status.success());

    let database = workspace.join(".agent-stride/agent-stride.db");
    let mut store = Store::open(&database).expect("open store");
    store.migrate().expect("migrate");
    let run_id = RunId::new("cli-handoff-run").expect("run");
    let task_id = TaskId::new("task-a").expect("task");
    store
        .start_run(
            run_id.clone(),
            "plan-1",
            RuntimeCommand::new(
                "start-cli-handoff",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            )
            .expect("command"),
        )
        .expect("start");
    store
        .apply_runtime_command(
            &run_id,
            RuntimeCommand::new(
                "reconcile-cli-handoff",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-1".to_owned(),
                    discovered: vec![task_id.clone()],
                    ready: vec![task_id],
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            )
            .expect("command"),
        )
        .expect("reconcile");
    let (lease, _) = store
        .poll_runtime_task(
            Actor::new("agent-a").expect("actor"),
            "2026-09-29T00:00:02Z",
            "2099-01-01T00:00:00Z",
        )
        .expect("poll");
    let token = store
        .issue_runtime_task_token(&lease)
        .expect("token")
        .encode()
        .expect("encode");

    let result = json(
        workspace,
        &[
            "--json",
            "worker",
            "handoff",
            &token,
            "--to-worker",
            "agent-b",
            "--summary",
            "research context is ready",
            "--artifacts",
            "notes.md",
            "--next-step",
            "validate source claims",
        ],
    );
    assert_eq!(result["state"]["tasks"]["task-a"]["status"], "scheduled");
    assert!(
        result["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| {
                event["kind"]["type"] == "task_handed_off"
                    && event["kind"]["to_worker"] == "agent-b"
            })
    );

    let (next_lease, _) = store
        .poll_runtime_task(
            Actor::new("agent-b").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:01:03Z",
        )
        .expect("target poll");
    assert_eq!(next_lease.attempt, 2);
}
