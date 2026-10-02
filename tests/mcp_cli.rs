use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn mcp_lists_tools_and_dispatches_ready() {
    let project = tempdir().expect("temporary project");
    Command::cargo_bin("as")
        .expect("binary should build")
        .arg("--workspace")
        .arg(project.path())
        .arg("init")
        .arg("--quiet")
        .assert()
        .success();

    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ready\",\"arguments\":{}}}\n"
    );
    let output = Command::cargo_bin("as")
        .expect("binary should build")
        .arg("--workspace")
        .arg(project.path())
        .arg("mcp")
        .write_stdin(input)
        .output()
        .expect("MCP should run");
    assert!(output.status.success());

    let lines = String::from_utf8(output.stdout)
        .expect("MCP output should be UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("each response should be JSON"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(lines[1]["result"]["tools"][0]["name"], "ready");
    assert_eq!(
        lines[2]["result"]["structuredContent"],
        serde_json::json!([])
    );
}
