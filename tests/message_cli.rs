use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn message_send_inbox_and_reply_preserve_thread() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["init", "--quiet"])
        .assert()
        .success();

    let sent = Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args([
            "send",
            "--from",
            "agent-a",
            "--to",
            "agent-b",
            "--subject",
            "Need input",
            "Please review",
            "--json",
        ])
        .output()
        .expect("send should run");
    assert!(sent.status.success());
    let message: Value = serde_json::from_slice(&sent.stdout).expect("message JSON");
    let thread = message["thread_id"].as_str().expect("thread id");

    Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args([
            "reply", thread, "--from", "agent-b", "--to", "agent-a", "Reviewed",
        ])
        .assert()
        .success();

    let inbox = Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["inbox", "--actor", "agent-b", "--json"])
        .output()
        .expect("inbox should run");
    let messages: Value = serde_json::from_slice(&inbox.stdout).expect("inbox JSON");
    assert_eq!(messages.as_array().expect("message list").len(), 1);

    let thread_output = Command::cargo_bin("ag")
        .expect("binary should build")
        .current_dir(project.path())
        .args(["thread", thread, "--json"])
        .output()
        .expect("thread should run");
    let messages: Value = serde_json::from_slice(&thread_output.stdout).expect("thread JSON");
    assert_eq!(messages.as_array().expect("thread list").len(), 2);
}
