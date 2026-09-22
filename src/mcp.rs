use std::{
    io::{self, BufRead, Write},
    path::Path,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Request {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

pub fn serve(workspace: Option<&Path>) -> Result<(), io::Error> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(request, workspace),
            Err(error) => Response {
                jsonrpc: "2.0",
                id: None,
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: format!("invalid JSON: {error}"),
                }),
            },
        };
        serde_json::to_writer(&mut stdout, &response).map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle(request: Request, workspace: Option<&Path>) -> Response {
    if request.jsonrpc != "2.0" {
        return error(request.id, -32600, "jsonrpc must be 2.0");
    }
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "agent-stride", "version": env!("CARGO_PKG_VERSION") }
        })),
        "notifications/initialized" => return empty(request.id),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(&request.params, workspace),
        _ => Err((-32601, format!("method not found: {}", request.method))),
    };
    match result {
        Ok(value) => Response {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        },
        Err((code, message)) => error(id, code, message),
    }
}

fn empty(id: Option<Value>) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: None,
        error: None,
    }
}

fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Response {
    Response {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
        }),
    }
}

fn tool_definitions() -> Value {
    json!([
        tool(
            "ready",
            "List executable work items.",
            json!({
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            })
        ),
        tool(
            "show",
            "Read one work item.",
            json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            })
        ),
        tool(
            "prime",
            "Build bounded resume context for an agent.",
            json!({
                "type": "object",
                "properties": {
                    "actor": { "type": "string" },
                    "max_memory_count": { "type": "integer" },
                    "max_memory_chars": { "type": "integer" }
                }
            })
        ),
        tool(
            "claim_next",
            "Atomically claim the highest-priority ready item.",
            json!({
                "type": "object",
                "required": ["actor"],
                "properties": {
                    "actor": { "type": "string" },
                    "lease_seconds": { "type": "integer" }
                }
            })
        ),
        tool(
            "checkpoint",
            "Save resumable progress for a claimed item.",
            json!({
                "type": "object",
                "required": ["id", "actor", "summary", "completed", "next"],
                "properties": {
                    "id": { "type": "string" },
                    "actor": { "type": "string" },
                    "summary": { "type": "string" },
                    "completed": { "type": "string" },
                    "next": { "type": "string" },
                    "blockers": { "type": "string" },
                    "evidence": { "type": "string" }
                }
            })
        ),
        tool(
            "close",
            "Close a work item.",
            json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" },
                    "reason": { "type": "string" }
                }
            })
        ),
        tool(
            "send",
            "Send a message to another agent.",
            json!({
                "type": "object",
                "required": ["from", "to", "body"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "subject": { "type": "string" },
                    "body": { "type": "string" },
                    "thread": { "type": "string" },
                    "parent": { "type": "string" },
                    "kind": { "type": "string" }
                }
            })
        ),
        tool(
            "inbox",
            "Read an agent inbox.",
            json!({
                "type": "object",
                "required": ["actor"],
                "properties": {
                    "actor": { "type": "string" },
                    "thread": { "type": "string" },
                    "mark_read": { "type": "boolean" }
                }
            })
        ),
        tool(
            "thread",
            "Read a message thread.",
            json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            })
        ),
        tool(
            "reply",
            "Reply in a message thread.",
            json!({
                "type": "object",
                "required": ["id", "from", "to", "body"],
                "properties": {
                    "id": { "type": "string" },
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "body": { "type": "string" }
                }
            })
        )
    ])
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn call_tool(params: &Value, workspace: Option<&Path>) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires name".to_owned()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args = arguments
        .as_object()
        .ok_or((-32602, "tools/call arguments must be an object".to_owned()))?;
    let mut cli_args = vec!["--json".to_owned()];
    if let Some(workspace) = workspace {
        cli_args.extend(["--workspace".to_owned(), workspace.display().to_string()]);
    }
    cli_args.extend(tool_args(name, args)?);
    let executable = std::env::current_exe().map_err(|error| (-32000, error.to_string()))?;
    let output = Command::new(executable)
        .args(cli_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| (-32000, error.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Ok(json!({
            "isError": true,
            "content": [{ "type": "text", "text": if stderr.is_empty() { stdout } else { stderr } }]
        }));
    }
    let structured = serde_json::from_str::<Value>(&stdout).unwrap_or_else(|_| json!(stdout));
    Ok(json!({
        "content": [{ "type": "text", "text": stdout }],
        "structuredContent": structured
    }))
}

fn tool_args(
    name: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, (i32, String)> {
    let string = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or((-32602, format!("missing string argument `{key}`")))
    };
    let optional_string = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut result = vec![name.replace('_', "-")];
    match name {
        "ready" => {
            if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
                result.extend(["--limit".to_owned(), limit.to_string()]);
            }
        }
        "show" => result.push(string("id")?),
        "prime" => {
            if let Some(actor) = optional_string("actor") {
                result.extend(["--actor".to_owned(), actor]);
            }
            if let Some(value) = args.get("max_memory_count").and_then(Value::as_u64) {
                result.extend(["--max-memory-count".to_owned(), value.to_string()]);
            }
            if let Some(value) = args.get("max_memory_chars").and_then(Value::as_u64) {
                result.extend(["--max-memory-chars".to_owned(), value.to_string()]);
            }
        }
        "claim_next" => {
            result.extend(["--actor".to_owned(), string("actor")?]);
            if let Some(value) = args.get("lease_seconds").and_then(Value::as_i64) {
                result.extend(["--lease-seconds".to_owned(), value.to_string()]);
            }
        }
        "checkpoint" => {
            result.push(string("id")?);
            for key in [
                "actor",
                "summary",
                "completed",
                "next",
                "blockers",
                "evidence",
            ] {
                if let Some(value) = optional_string(key) {
                    result.extend([format!("--{key}"), value]);
                }
            }
        }
        "close" => {
            result.push(string("id")?);
            if let Some(reason) = optional_string("reason") {
                result.extend(["--reason".to_owned(), reason]);
            }
        }
        "send" => {
            result.extend([
                "--from".to_owned(),
                string("from")?,
                "--to".to_owned(),
                string("to")?,
                string("body")?,
            ]);
            if let Some(subject) = optional_string("subject") {
                result.extend(["--subject".to_owned(), subject]);
            }
            if let Some(thread) = optional_string("thread") {
                result.extend(["--thread".to_owned(), thread]);
            }
            if let Some(parent) = optional_string("parent") {
                result.extend(["--parent".to_owned(), parent]);
            }
            if let Some(kind) = optional_string("kind") {
                result.extend(["--kind".to_owned(), kind]);
            }
        }
        "inbox" => {
            result.extend(["--actor".to_owned(), string("actor")?]);
            if let Some(thread) = optional_string("thread") {
                result.extend(["--thread".to_owned(), thread]);
            }
            if args.get("mark_read").and_then(Value::as_bool) == Some(true) {
                result.push("--mark-read".to_owned());
            }
        }
        "thread" => result.push(string("id")?),
        "reply" => {
            result.extend([
                string("id")?,
                "--from".to_owned(),
                string("from")?,
                "--to".to_owned(),
                string("to")?,
                string("body")?,
            ]);
        }
        _ => return Err((-32602, format!("unknown tool `{name}`"))),
    }
    Ok(result)
}
