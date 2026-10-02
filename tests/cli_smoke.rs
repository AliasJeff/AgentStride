use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn help_is_available() {
    let mut command = Command::cargo_bin("as").expect("binary should build");

    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("long-running Agent work"));
}

#[test]
fn init_creates_an_idempotent_workspace() {
    let project = tempdir().expect("temporary project");

    let mut first = Command::cargo_bin("as").expect("binary should build");
    first
        .current_dir(project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized workspace"));

    assert!(
        project
            .path()
            .join(".agent-stride/agent-stride.db")
            .is_file()
    );

    let mut second = Command::cargo_bin("as").expect("binary should build");
    second
        .current_dir(project.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("already initialized"));
}

#[test]
fn init_supports_an_explicit_project_path() {
    let project = tempdir().expect("temporary project");
    let expected = project.path().join(".agent-stride");
    let mut command = Command::cargo_bin("as").expect("binary should build");

    command
        .arg("--workspace")
        .arg(project.path())
        .arg("init")
        .assert()
        .success();

    assert!(expected.join("agent-stride.db").is_file());
}
