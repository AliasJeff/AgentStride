use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(workspace: &Path, cache: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ag"))
        .env("XDG_CACHE_HOME", cache)
        .args(["--workspace", workspace.to_str().expect("workspace")])
        .args(args)
        .output()
        .expect("run ag")
}

fn json(workspace: &Path, cache: &Path, args: &[&str]) -> Value {
    let output = run(workspace, cache, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json output")
}

#[test]
fn post_compact_refreshes_once_on_the_next_prompt() {
    let project = TempDir::new().expect("project");
    let cache = TempDir::new().expect("cache");
    let workspace = project.path();
    assert!(run(workspace, cache.path(), &["init"]).status.success());

    let start = json(
        workspace,
        cache.path(),
        &["agent-hook", "SessionStart", "--session", "s1", "--json"],
    );
    assert!(start["refresh"].as_bool().expect("refresh"));

    let compacted = json(
        workspace,
        cache.path(),
        &["agent-hook", "PostCompact", "--session", "s1", "--json"],
    );
    assert!(!compacted["refresh"].as_bool().expect("refresh"));

    let first = json(
        workspace,
        cache.path(),
        &[
            "agent-hook",
            "UserPromptSubmit",
            "--session",
            "s1",
            "--json",
        ],
    );
    assert!(first["refresh"].as_bool().expect("refresh"));
    assert!(first["context"].is_object());

    let second = json(
        workspace,
        cache.path(),
        &[
            "agent-hook",
            "UserPromptSubmit",
            "--session",
            "s1",
            "--json",
        ],
    );
    assert!(!second["refresh"].as_bool().expect("refresh"));
    assert!(second["context"].is_null());
}
