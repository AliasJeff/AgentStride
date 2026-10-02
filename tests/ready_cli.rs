use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn json_command(project: &std::path::Path, args: &[&str]) -> Value {
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

fn add(project: &std::path::Path, title: &str) -> String {
    json_command(project, &["add", title, "--json"])["id"]
        .as_str()
        .expect("issue id")
        .to_owned()
}

#[test]
fn closing_a_blocker_changes_the_ready_frontier() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let blocker = add(project.path(), "Design storage");
    let dependent = add(project.path(), "Implement storage");
    json_command(
        project.path(),
        &["link", &dependent, "--blocks", &blocker, "--json"],
    );

    let ready = json_command(project.path(), &["ready", "--json"]);
    let ready_ids: Vec<&str> = ready
        .as_array()
        .expect("ready list")
        .iter()
        .map(|issue| issue["id"].as_str().expect("ready issue id"))
        .collect();
    assert!(ready_ids.contains(&blocker.as_str()));
    assert!(!ready_ids.contains(&dependent.as_str()));

    json_command(
        project.path(),
        &["close", &blocker, "--reason", "design accepted", "--json"],
    );

    let ready = json_command(project.path(), &["ready", "--json"]);
    let ready_ids: Vec<&str> = ready
        .as_array()
        .expect("ready list")
        .iter()
        .map(|issue| issue["id"].as_str().expect("ready issue id"))
        .collect();
    assert!(ready_ids.contains(&dependent.as_str()));
}
