use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn marking_inbox_read_is_replayed() {
    let source = tempdir().expect("source workspace");
    let replayed = tempdir().expect("replayed workspace");
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(source.path())
        .args(["init", "--quiet"])
        .assert()
        .success();
    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(source.path())
        .args([
            "send",
            "--from",
            "agent-a",
            "--to",
            "agent-b",
            "Please review",
            "--quiet",
        ])
        .assert()
        .success();

    let inbox = Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(source.path())
        .args(["inbox", "--actor", "agent-b", "--json"])
        .output()
        .expect("inbox should run");
    let messages: Value = serde_json::from_slice(&inbox.stdout).expect("inbox JSON");
    let message_id = messages[0]["id"].as_str().expect("message id");

    Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(source.path())
        .args(["inbox", "--actor", "agent-b", "--mark-read", "--quiet"])
        .assert()
        .success();

    let events = Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(source.path())
        .args(["events", "--since", "0", "--limit", "100", "--json"])
        .output()
        .expect("events should run");
    let events: Value = serde_json::from_slice(&events.stdout).expect("events JSON");
    let event = events["events"]
        .as_array()
        .expect("event list")
        .iter()
        .find(|event| event["operation"] == "message_read")
        .expect("message_read event");
    assert_eq!(event["payload"]["message_id"], message_id);

    let export = source.path().join("events.jsonl");
    let event_lines = events["events"]
        .as_array()
        .expect("event list")
        .iter()
        .map(|event| serde_json::to_string(event).expect("serialize event"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&export, event_lines).expect("write events");

    Command::cargo_bin("as")
        .expect("binary should build")
        .args(["replay", export.to_str().expect("path"), "--into"])
        .arg(replayed.path())
        .assert()
        .success();

    let recovered_inbox = Command::cargo_bin("as")
        .expect("binary should build")
        .current_dir(replayed.path())
        .args(["inbox", "--actor", "agent-b", "--json"])
        .output()
        .expect("replayed inbox should run");
    let recovered: Value =
        serde_json::from_slice(&recovered_inbox.stdout).expect("replayed inbox JSON");
    assert_eq!(recovered[0]["id"], message_id);
    assert!(
        recovered[0]["read_at"].as_str().is_some(),
        "replayed message should retain read state"
    );
}
