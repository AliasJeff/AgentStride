use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(_root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ag"))
        .args(args)
        .output()
        .expect("run ag")
}

fn json(root: &Path, args: &[&str]) -> Value {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

#[test]
fn setup_is_idempotent_and_preserves_user_rules() {
    let root = TempDir::new().expect("root");
    fs::write(root.path().join("AGENTS.md"), "user-owned rules\n").expect("write");

    let first = json(
        root.path(),
        &[
            "--json",
            "setup",
            "generic",
            "--root",
            root.path().to_str().expect("path"),
        ],
    );
    assert!(first["changed"].as_bool().expect("changed"));

    let second = json(
        root.path(),
        &[
            "--json",
            "setup",
            "generic",
            "--root",
            root.path().to_str().expect("path"),
        ],
    );
    assert!(!second["changed"].as_bool().expect("changed"));
    assert!(
        fs::read_to_string(root.path().join("AGENTS.md"))
            .expect("read")
            .contains("user-owned rules")
    );

    let check = json(
        root.path(),
        &[
            "--json",
            "setup",
            "generic",
            "--root",
            root.path().to_str().expect("path"),
            "--check",
        ],
    );
    assert!(!check["drifted"].as_bool().expect("drifted"));

    let removed = json(
        root.path(),
        &[
            "--json",
            "setup",
            "generic",
            "--root",
            root.path().to_str().expect("path"),
            "--remove",
        ],
    );
    assert!(removed["removed"].as_bool().expect("removed"));
    assert!(
        !root
            .path()
            .join(".agents/skills/agent-stride/SKILL.md")
            .exists()
    );
}
