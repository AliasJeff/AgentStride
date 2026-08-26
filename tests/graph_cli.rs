use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

fn command(project: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("ag")
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

fn add(project: &std::path::Path, title: &str) -> String {
    json_command(project, &["add", title, "--json"])["id"]
        .as_str()
        .expect("issue id")
        .to_owned()
}

#[test]
fn link_graph_and_unlink_workflow() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let blocker = add(project.path(), "Design storage");
    let dependent = add(project.path(), "Implement storage");

    let linked = json_command(
        project.path(),
        &["link", &dependent, "--blocks", &blocker, "--json"],
    );
    assert_eq!(linked["edge_type"], "blocks");

    let graph = json_command(project.path(), &["graph", "--json"]);
    assert_eq!(graph.as_array().expect("edge list").len(), 1);

    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["unlink", &dependent, &blocker])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed dependency"));

    let graph = json_command(project.path(), &["graph", "--json"]);
    assert!(graph.as_array().expect("edge list").is_empty());
}

#[test]
fn link_rejects_cycle() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let first = add(project.path(), "First");
    let second = add(project.path(), "Second");
    json_command(
        project.path(),
        &["link", &second, "--blocks", &first, "--json"],
    );

    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["link", &first, "--blocks", &second, "--json"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("dependency cycle"));
}

#[test]
fn epic_reports_child_progress() {
    let project = tempdir().expect("temporary project");
    assert!(
        command(project.path(), &["init", "--quiet"])
            .status
            .success()
    );
    let epic = json_command(
        project.path(),
        &["add", "Release 1", "--kind", "epic", "--json"],
    );
    let epic_id = epic["id"].as_str().expect("epic id").to_owned();
    let child = add(project.path(), "Implement feature");
    json_command(
        project.path(),
        &["link", &child, "--parent", &epic_id, "--json"],
    );

    let summary = json_command(project.path(), &["epic", &epic_id, "--json"]);
    assert_eq!(summary["epic"]["kind"], "epic");
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["completed"], 0);
    assert_eq!(summary["complete"], false);

    assert!(
        command(
            project.path(),
            &["claim", &child, "--actor", "agent-a", "--json"]
        )
        .status
        .success()
    );
    assert!(
        command(project.path(), &["close", &child, "--json"])
            .status
            .success()
    );
    let summary = json_command(project.path(), &["epic", &epic_id, "--json"]);
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["complete"], true);
}

#[test]
fn epic_is_a_container_and_not_ready_for_claiming() {
    let project = tempdir().expect("temporary project");
    assert!(
        command(project.path(), &["init", "--quiet"])
            .status
            .success()
    );
    let epic = json_command(
        project.path(),
        &["add", "Release 1", "--kind", "epic", "--json"],
    );
    let epic_id = epic["id"].as_str().expect("epic id");
    let ready = json_command(project.path(), &["ready", "--json"]);
    assert!(
        ready
            .as_array()
            .expect("ready list")
            .iter()
            .all(|issue| issue["id"] != epic_id)
    );
    assert!(
        command(
            project.path(),
            &["claim", epic_id, "--actor", "agent-a", "--json"]
        )
        .status
        .code()
            == Some(1)
    );
}
