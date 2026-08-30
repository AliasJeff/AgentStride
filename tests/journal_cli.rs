use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn json_command(project: &std::path::Path, args: &[&str]) -> Value {
    let output = Command::cargo_bin("ag")
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
fn lifecycle_mutations_are_visible_through_a_cursor() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let created = json_command(project.path(), &["add", "Journal task", "--json"]);
    let id = created["id"].as_str().expect("issue id").to_owned();

    json_command(
        project.path(),
        &["update", &id, "--title", "Updated task", "--json"],
    );
    json_command(
        project.path(),
        &["claim", &id, "--actor", "worker-a", "--json"],
    );
    json_command(
        project.path(),
        &["close", &id, "--reason", "done", "--json"],
    );

    let page = json_command(project.path(), &["events", "--since", "0", "--json"]);
    assert_eq!(page["head"], 4);
    let events = page["events"].as_array().expect("event page");
    assert_eq!(events.len(), 4);
    assert_eq!(
        events
            .iter()
            .map(|event| event["operation"].as_str().expect("operation"))
            .collect::<Vec<_>>(),
        [
            "issue_created",
            "issue_updated",
            "issue_claimed",
            "issue_closed"
        ]
    );
    assert_eq!(events[2]["actor"], "worker-a");
    assert_eq!(events[3]["snapshot"]["status"], "closed");
    assert_eq!(events[3]["snapshot"]["close_reason"], "done");

    let tail = json_command(
        project.path(),
        &["events", "--since", "2", "--limit", "1", "--json"],
    );
    assert_eq!(tail["head"], 4);
    assert_eq!(tail["events"].as_array().expect("tail events").len(), 1);
    assert_eq!(tail["events"][0]["seq"], 3);
}

#[test]
fn edge_mutations_are_journaled_with_payloads() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let blocker = json_command(project.path(), &["add", "Blocker", "--json"])["id"]
        .as_str()
        .expect("blocker id")
        .to_owned();
    let dependent = json_command(project.path(), &["add", "Dependent", "--json"])["id"]
        .as_str()
        .expect("dependent id")
        .to_owned();
    json_command(
        project.path(),
        &["link", &dependent, "--blocks", &blocker, "--json"],
    );
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["unlink", &dependent, &blocker])
        .assert()
        .success();

    let page = json_command(project.path(), &["events", "--json"]);
    let events = page["events"].as_array().expect("event page");
    assert_eq!(events.len(), 4);
    assert_eq!(events[2]["operation"], "edge_created");
    assert_eq!(events[2]["payload"]["edge_type"], "blocks");
    assert_eq!(events[3]["operation"], "edge_removed");
    assert_eq!(events[3]["payload"]["source_id"], dependent);
}
