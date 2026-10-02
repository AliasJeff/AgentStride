use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use agent_stride::{
    runtime::{Command as RuntimeCommand, CommandKind, RunId},
    store::Store,
};
use tempfile::TempDir;

fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("free port");
    listener.local_addr().expect("address").port()
}

fn start_ui(project: &TempDir, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_as"))
        .args([
            "--workspace",
            project.path().to_str().expect("project"),
            "ui",
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start UI")
}

fn request(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect UI");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    response
}

fn wait_for_ui(port: u16) {
    for _ in 0..50 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .write_all(
                    b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .expect("health request");
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("UI did not start on port {port}");
}

fn body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP body")
}

#[test]
fn ui_serves_static_page_and_runtime_json_without_mutating_history() {
    let project = TempDir::new().expect("project");
    Command::new(env!("CARGO_BIN_EXE_as"))
        .args([
            "--workspace",
            project.path().to_str().expect("project"),
            "init",
            "--quiet",
        ])
        .status()
        .expect("initialize")
        .success()
        .then_some(())
        .expect("init succeeded");

    let database = project.path().join(".agent-stride/agent-stride.db");
    let mut store = Store::open(&database).expect("open store");
    store.migrate().expect("migrate");
    let run_id = RunId::new("ui-run").expect("run id");
    store
        .start_run(
            run_id.clone(),
            "plan-ui",
            RuntimeCommand::new(
                "start-ui-run",
                CommandKind::StartRun {
                    now: "2026-09-30T00:00:00Z".to_owned(),
                },
            )
            .expect("command"),
        )
        .expect("start run");
    let before = store.runtime_state(&run_id).expect("state").history_version;
    drop(store);

    let port = free_port();
    let mut child = start_ui(&project, port);
    wait_for_ui(port);

    let page = request(port, "/");
    assert!(page.contains("Execution flow"));
    let health = request(port, "/api/health");
    assert!(body(&health).contains("\"status\":\"ok\""));
    let runs = request(port, "/api/runs");
    assert!(body(&runs).contains("\"run_id\":\"ui-run\""));
    let overview = request(port, "/api/runs/ui-run/overview");
    assert!(body(&overview).contains("\"history_version\":1"));
    let missing = request(port, "/api/runs/missing/overview");
    assert!(missing.starts_with("HTTP/1.1 404"));
    assert!(body(&missing).contains("\"kind\":\"runtime_run_not_found\""));
    let invalid_limit = request(port, "/api/runs/ui-run/events?limit=501");
    assert!(invalid_limit.starts_with("HTTP/1.1 400"));
    assert!(body(&invalid_limit).contains("\"kind\":\"invalid_request\""));

    child.kill().expect("stop UI");
    child.wait().expect("wait UI");
    let store = Store::open(&database).expect("reopen store");
    assert_eq!(
        store
            .runtime_state(&run_id)
            .expect("state after UI reads")
            .history_version,
        before
    );
}
