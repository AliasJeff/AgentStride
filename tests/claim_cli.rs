use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

fn command(project: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project)
        .args(args)
        .output()
        .expect("command should run")
}

fn json_command(project: &std::path::Path, args: &[&str]) -> Value {
    let output = command(project, args);
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command should emit JSON")
}

#[test]
fn claim_next_assigns_ready_work_and_rejects_foreign_worker() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let created = json_command(
        project.path(),
        &["add", "Implement claiming", "--priority", "1", "--json"],
    );
    let id = created["id"].as_str().expect("issue id").to_owned();

    let claimed = json_command(
        project.path(),
        &["claim-next", "--actor", "worker-a", "--json"],
    );
    assert_eq!(claimed["id"], id);
    assert_eq!(claimed["status"], "in_progress");
    assert_eq!(claimed["assignee"], "worker-a");
    assert_eq!(claimed["revision"], 2);

    let retry = json_command(
        project.path(),
        &["claim", &id, "--actor", "worker-a", "--json"],
    );
    assert_eq!(retry["revision"], 2);

    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["claim", &id, "--actor", "worker-b", "--json"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "issue is already claimed by `worker-a`",
        ));
}

#[test]
fn update_revision_guard_rejects_stale_cli_writes() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let created = json_command(project.path(), &["add", "Revision guard", "--json"]);
    let id = created["id"].as_str().expect("issue id");

    json_command(
        project.path(),
        &[
            "update",
            id,
            "--title",
            "Fresh write",
            "--if-revision",
            "1",
            "--json",
        ],
    );

    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args([
            "update",
            id,
            "--title",
            "Stale write",
            "--if-revision",
            "1",
            "--json",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("revision mismatch: expected 1"));
}
