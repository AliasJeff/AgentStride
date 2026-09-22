use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::{
    plan::{GitRepository, PlanError, PlanGraph},
    runtime::{RunId, TaskStatus},
    store::{RuntimeRunSummary, Store, StoreError},
};

const INDEX_HTML: &str = include_str!("../../ui/index.html");
const APP_JS: &str = include_str!("../../ui/app.js");
const STYLES_CSS: &str = include_str!("../../ui/styles.css");
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";
const JS_CONTENT_TYPE: &str = "text/javascript; charset=utf-8";
const CSS_CONTENT_TYPE: &str = "text/css; charset=utf-8";
const MAX_EVENT_LIMIT: usize = 500;

#[derive(Debug, Error)]
pub enum WebError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("invalid UI request: {message}")]
    InvalidRequest { message: String },
    #[error("failed to start UI server: {message}")]
    Server { message: String },
}

#[derive(Debug, Serialize)]
struct Envelope<T> {
    schema_version: u8,
    data: T,
    meta: Meta,
}

#[derive(Debug, Serialize)]
struct Meta {
    generated_at: String,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    runtime: &'static str,
}

#[derive(Debug, Serialize)]
struct Overview {
    summary: RuntimeRunSummary,
    tasks: Vec<TaskView>,
    last_event_at: Option<String>,
    recovery_events: usize,
    current_revision: String,
}

#[derive(Debug, Serialize)]
struct TaskView {
    task_id: String,
    status: &'static str,
    attempt: u32,
    worker: Option<String>,
    checkpoint: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct PlanView {
    revision: String,
    issues: Vec<PlanIssueView>,
    edges: Vec<PlanEdgeView>,
}

#[derive(Debug, Serialize)]
struct PlanIssueView {
    id: String,
    title: String,
    kind: &'static str,
    status: &'static str,
    priority: u8,
    runtime_status: Option<&'static str>,
    worker: Option<String>,
    attempt: Option<u32>,
}

#[derive(Debug, Serialize)]
struct PlanEdgeView {
    source_id: String,
    target_id: String,
    edge_type: &'static str,
}

#[derive(Debug, Deserialize, Default)]
struct Query {
    since: Option<u64>,
    limit: Option<usize>,
}

pub fn serve(workspace: &Path, default_run: Option<&str>, port: u16) -> Result<(), WebError> {
    let server = Server::http(("127.0.0.1", port)).map_err(|error| WebError::Server {
        message: error.to_string(),
    })?;
    println!("AgentStride UI listening on http://127.0.0.1:{port}");

    for request in server.incoming_requests() {
        let (request, response) = handle_request(request, workspace, default_run);
        if let Err(error) = request.respond(response) {
            eprintln!("UI response failed: {error}");
        }
    }
    Ok(())
}

fn handle_request(
    request: Request,
    workspace: &Path,
    default_run: Option<&str>,
) -> HandledResponse {
    let response = handle_route(&request, workspace, default_run)
        .unwrap_or_else(|error| error_response(&error));
    (request, response)
}

fn handle_route(
    request: &Request,
    workspace: &Path,
    default_run: Option<&str>,
) -> Result<Response<std::io::Cursor<Vec<u8>>>, WebError> {
    let method = request.method().clone();
    let (path, query_string) = split_url(request.url());
    if method != Method::Get {
        return Ok(text_response(
            405,
            "method not allowed",
            "text/plain; charset=utf-8",
        ));
    }

    match path {
        "/" | "/index.html" => {
            let default_run =
                serde_json::to_string(&default_run).map_err(|error| WebError::InvalidRequest {
                    message: format!("failed to encode default run: {error}"),
                })?;
            let body = INDEX_HTML.replace("__AG_DEFAULT_RUN__", &default_run);
            Ok(body_response(200, body, HTML_CONTENT_TYPE))
        }
        "/app.js" => Ok(body_response(200, APP_JS, JS_CONTENT_TYPE)),
        "/styles.css" => Ok(body_response(200, STYLES_CSS, CSS_CONTENT_TYPE)),
        "/api/health" => Ok(json_response(
            200,
            &Envelope {
                schema_version: 1,
                data: Health {
                    status: "ok",
                    runtime: "agent-stride",
                },
                meta: meta(),
            },
        )),
        "/api/runs" => {
            let store = open_store(workspace)?;
            let data = store.list_runtime_runs()?;
            Ok(json_envelope(200, data)?)
        }
        path if path.starts_with("/api/runs/") => {
            let parts = path
                .trim_start_matches("/api/runs/")
                .split('/')
                .collect::<Vec<_>>();
            if parts.len() < 2 {
                return Err(WebError::InvalidRequest {
                    message: "run endpoint requires a run id and resource".to_owned(),
                });
            }
            let run_id =
                RunId::new(parts[0].to_owned()).map_err(|error| WebError::InvalidRequest {
                    message: error.to_string(),
                })?;
            let store = open_store(workspace)?;
            match parts[1] {
                "overview" if parts.len() == 2 => {
                    Ok(json_envelope(200, overview(&store, &run_id)?)?)
                }
                "events" if parts.len() == 2 => {
                    let query = parse_query(query_string)?;
                    let since = query.since.unwrap_or(0);
                    let limit = query.limit.unwrap_or(100);
                    if limit == 0 || limit > MAX_EVENT_LIMIT {
                        return Err(WebError::InvalidRequest {
                            message: format!("limit must be between 1 and {MAX_EVENT_LIMIT}"),
                        });
                    }
                    let page = store.runtime_events(&run_id, since, limit)?;
                    Ok(json_envelope(200, page)?)
                }
                "plan" if parts.len() == 2 => {
                    Ok(json_envelope(200, plan_view(workspace, &store, &run_id)?)?)
                }
                _ => Err(WebError::InvalidRequest {
                    message: "unknown run resource".to_owned(),
                }),
            }
        }
        _ => Ok(text_response(404, "not found", "text/plain; charset=utf-8")),
    }
}

type HandledResponse = (Request, Response<std::io::Cursor<Vec<u8>>>);

fn open_store(workspace: &Path) -> Result<Store, WebError> {
    Ok(Store::open(&workspace.join("agent-stride.db"))?)
}

fn overview(store: &Store, run_id: &RunId) -> Result<Overview, WebError> {
    let state = store.runtime_state(run_id)?;
    let events = store.runtime_events(run_id, state.history_version.saturating_sub(50), 50)?;
    let summary = summarize_state(&state);
    let tasks = state
        .tasks
        .values()
        .map(|task| TaskView {
            task_id: task.task_id.to_string(),
            status: task_status(task.status),
            attempt: task.attempt,
            worker: task.worker.as_ref().map(|actor| actor.as_str().to_owned()),
            checkpoint: task.checkpoint.clone(),
            result: task.result.clone(),
            failure: task.failure.clone(),
        })
        .collect();
    let recovery_events = events
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                crate::runtime::EventKind::RetryScheduled { .. }
                    | crate::runtime::EventKind::TaskTimedOut { .. }
            )
        })
        .count();
    Ok(Overview {
        current_revision: state.plan_revision.clone(),
        summary,
        tasks,
        last_event_at: events.events.last().map(|event| event.occurred_at.clone()),
        recovery_events,
    })
}

fn summarize_state(state: &crate::runtime::MutableState) -> RuntimeRunSummary {
    let mut summary = RuntimeRunSummary {
        run_id: state.run_id.clone(),
        plan_revision: state.plan_revision.clone(),
        status: state.status,
        history_version: state.history_version,
        task_count: state.tasks.len(),
        succeeded_tasks: 0,
        active_tasks: 0,
        waiting_tasks: 0,
        failed_tasks: 0,
    };
    for task in state.tasks.values() {
        match task.status {
            TaskStatus::Succeeded => summary.succeeded_tasks += 1,
            TaskStatus::Running | TaskStatus::Claimed | TaskStatus::Scheduled => {
                summary.active_tasks += 1
            }
            TaskStatus::Waiting => summary.waiting_tasks += 1,
            TaskStatus::Failed => summary.failed_tasks += 1,
            _ => {}
        }
    }
    summary
}

fn plan_view(workspace: &Path, store: &Store, run_id: &RunId) -> Result<PlanView, WebError> {
    let state = store.runtime_state(run_id)?;
    let repository =
        GitRepository::new(workspace.parent().ok_or_else(|| WebError::InvalidRequest {
            message: "workspace has no project root".to_owned(),
        })?);
    let graph = PlanGraph::from_git(&repository, &state.plan_revision)?;
    let issues = graph
        .issues
        .values()
        .map(|issue| {
            let task = state
                .tasks
                .get(&crate::runtime::TaskId::new(issue.id.to_string()).expect("issue id"));
            PlanIssueView {
                id: issue.id.to_string(),
                title: issue.title.clone(),
                kind: issue.kind.as_str(),
                status: issue.status.as_str(),
                priority: issue.priority.value(),
                runtime_status: task.map(|task| task_status(task.status)),
                worker: task
                    .and_then(|task| task.worker.as_ref())
                    .map(|actor| actor.as_str().to_owned()),
                attempt: task.map(|task| task.attempt),
            }
        })
        .collect();
    let edges = graph
        .edges
        .iter()
        .map(|edge| PlanEdgeView {
            source_id: edge.source_id.to_string(),
            target_id: edge.target_id.to_string(),
            edge_type: edge.edge_type.as_str(),
        })
        .collect();
    Ok(PlanView {
        revision: state.plan_revision,
        issues,
        edges,
    })
}

fn task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Ready => "ready",
        TaskStatus::Scheduled => "scheduled",
        TaskStatus::Claimed => "claimed",
        TaskStatus::Running => "running",
        TaskStatus::Waiting => "waiting",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::Failed => "failed",
        TaskStatus::Invalidated => "invalidated",
    }
}

fn split_url(url: &str) -> (&str, &str) {
    url.split_once('?').unwrap_or((url, ""))
}

fn parse_query(value: &str) -> Result<Query, WebError> {
    if value.is_empty() {
        return Ok(Query::default());
    }
    let mut pairs = BTreeMap::new();
    for item in value.split('&') {
        let (key, value) = item.split_once('=').unwrap_or((item, ""));
        pairs.insert(key, value);
    }
    let since = pairs
        .get("since")
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| WebError::InvalidRequest {
            message: "since must be an unsigned integer".to_owned(),
        })?;
    let limit = pairs
        .get("limit")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| WebError::InvalidRequest {
            message: "limit must be an unsigned integer".to_owned(),
        })?;
    Ok(Query { since, limit })
}

fn json_envelope<T: Serialize>(
    status: u16,
    data: T,
) -> Result<Response<std::io::Cursor<Vec<u8>>>, WebError> {
    Ok(json_response(
        status,
        &Envelope {
            schema_version: 1,
            data,
            meta: meta(),
        },
    ))
}

fn meta() -> Meta {
    Meta {
        generated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_owned()),
    }
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|error| {
        format!(r#"{{"error":"failed to encode response: {error}"}}"#).into_bytes()
    });
    body_response(status, body, JSON_CONTENT_TYPE)
}

fn error_response(error: &WebError) -> Response<std::io::Cursor<Vec<u8>>> {
    let status = match error {
        WebError::InvalidRequest { .. } => 400,
        WebError::Store(StoreError::RuntimeRunNotFound { .. }) => 404,
        WebError::Plan(_) => 422,
        WebError::Server { .. } | WebError::Store(_) => 500,
    };
    json_response(
        status,
        &serde_json::json!({
            "schema_version": 1,
            "error": {
                "kind": error_kind(error),
                "message": error.to_string(),
            },
            "meta": meta(),
        }),
    )
}

fn error_kind(error: &WebError) -> &'static str {
    match error {
        WebError::Store(StoreError::RuntimeRunNotFound { .. }) => "runtime_run_not_found",
        WebError::Store(_) => "storage_error",
        WebError::Plan(_) => "plan_error",
        WebError::InvalidRequest { .. } => "invalid_request",
        WebError::Server { .. } => "server_error",
    }
}

fn body_response(
    status: u16,
    body: impl Into<Vec<u8>>,
    content_type: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.into())
        .with_status_code(StatusCode(status))
        .with_header(content_header(content_type))
}

fn text_response(
    status: u16,
    body: &str,
    content_type: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    body_response(status, body.as_bytes().to_vec(), content_type)
}

fn content_header(value: &str) -> Header {
    Header::from_bytes("Content-Type", value).expect("static content type header")
}
