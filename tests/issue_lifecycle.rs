use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn run_json(project: &std::path::Path, args: &[&str]) -> Value {
    let output = Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project)
        .args(args)
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command should emit JSON")
}

#[test]
fn issue_lifecycle_persists_across_commands() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let created = run_json(
        project.path(),
        &["add", "Design storage", "--priority", "1", "--json"],
    );
    let id = created["id"].as_str().expect("created issue id").to_owned();
    assert_eq!(created["status"], "open");
    assert_eq!(created["priority"], 1);

    let listed = run_json(project.path(), &["list", "--json"]);
    assert_eq!(listed.as_array().expect("issue list").len(), 1);

    let updated = run_json(
        project.path(),
        &[
            "update",
            &id,
            "--title",
            "Design SQLite storage",
            "--priority",
            "0",
            "--json",
        ],
    );
    assert_eq!(updated["title"], "Design SQLite storage");
    assert_eq!(updated["priority"], 0);

    let closed = run_json(
        project.path(),
        &["close", &id, "--reason", "schema accepted", "--json"],
    );
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["close_reason"], "schema accepted");

    let open_items = run_json(project.path(), &["list", "--status", "open", "--json"]);
    assert!(open_items.as_array().expect("filtered list").is_empty());
}

#[test]
fn commands_fail_without_an_initialized_workspace() {
    let project = tempdir().expect("temporary project");

    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["list", "--json"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            r#""kind":"workspace_not_initialized""#,
        ));
}
