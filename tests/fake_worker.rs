use serde_json::json;

use agent_stride::{
    domain::Actor,
    runtime::{
        Command, CommandKind, RunId, TaskId, TaskStatus, agent_worker::FakeWorker,
        agent_worker::FakeWorkerConfig,
    },
    store::{Store, StoreError},
};

fn command(request_id: &str, kind: CommandKind) -> Command {
    Command::new(request_id, kind).expect("valid command")
}

fn setup() -> (Store, RunId) {
    setup_with_tasks(&["task-a", "task-b"])
}

fn setup_with_tasks(task_names: &[&str]) -> (Store, RunId) {
    let mut store = Store::open_in_memory().expect("open store");
    store.migrate().expect("migrate store");
    let run_id = RunId::new("fake-worker-run").expect("run id");
    store
        .start_run(
            run_id.clone(),
            "plan-1",
            command(
                "start-fake-worker-run",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ),
        )
        .expect("start run");
    store
        .apply_runtime_command(
            &run_id,
            command(
                "reconcile-fake-worker-run",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-1".to_owned(),
                    discovered: task_names
                        .iter()
                        .map(|name| TaskId::new(*name).expect("task"))
                        .collect(),
                    ready: task_names
                        .iter()
                        .map(|name| TaskId::new(*name).expect("task"))
                        .collect(),
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ),
        )
        .expect("reconcile");
    (store, run_id)
}

#[test]
fn concurrent_workers_receive_distinct_durable_tasks() {
    let (mut store, run_id) = setup();
    let (first, _) = store
        .poll_runtime_task(
            Actor::new("worker-a").expect("actor"),
            "2026-09-29T00:00:02Z",
            "2026-09-29T00:01:02Z",
        )
        .expect("first poll");
    let (second, _) = store
        .poll_runtime_task(
            Actor::new("worker-b").expect("actor"),
            "2026-09-29T00:00:02Z",
            "2026-09-29T00:01:02Z",
        )
        .expect("second poll");

    assert_ne!(first.durable_task_id, second.durable_task_id);
    assert_ne!(first.task_id, second.task_id);
    assert_eq!(
        store.runtime_state(&run_id).expect("state").history_version,
        9
    );
}

#[test]
fn deterministic_worker_failure_is_retried_without_accepting_a_stale_token() {
    let (mut store, run_id) = setup_with_tasks(&["task-a"]);
    let worker = FakeWorker::new(FakeWorkerConfig {
        seed: 17,
        failures_before_success: 1,
        crash_at_step: None,
        duration_steps: 0,
        heartbeat_every: None,
        proposal_id: None,
        handoff_to: None,
    });
    let (lease, _) = store
        .poll_runtime_task(
            Actor::new("worker-a").expect("actor"),
            "2026-09-29T00:00:02Z",
            "2026-09-29T00:01:02Z",
        )
        .expect("poll");
    let token = store.issue_runtime_task_token(&lease).expect("token");
    store
        .start_runtime_task(&token, "2026-09-29T00:00:03Z")
        .expect("start");
    assert!(matches!(
        worker.outcome(lease.task_id.as_str(), 1, 0),
        agent_stride::runtime::agent_worker::AgentOutcome::Failed { .. }
    ));

    store
        .timeout_runtime_task(
            &lease.durable_task_id,
            agent_stride::runtime::TimerId::new("retry-timer").expect("timer"),
            "2026-09-29T00:02:00Z",
            "2026-09-29T00:01:03Z",
        )
        .expect("timeout");
    store
        .fire_runtime_timer(
            agent_stride::runtime::TimerId::new("retry-timer").expect("timer"),
            "2026-09-29T00:02:01Z",
        )
        .expect("fire timer");

    let (retry, _) = store
        .poll_runtime_task(
            Actor::new("worker-c").expect("actor"),
            "2026-09-29T00:02:02Z",
            "2026-09-29T00:03:02Z",
        )
        .expect("retry poll");
    assert_ne!(retry.durable_task_id, lease.durable_task_id);
    let retry_token = store.issue_runtime_task_token(&retry).expect("retry token");
    store
        .start_runtime_task(&retry_token, "2026-09-29T00:02:03Z")
        .expect("retry start");
    assert!(matches!(
        worker.outcome(retry.task_id.as_str(), 2, 0),
        agent_stride::runtime::agent_worker::AgentOutcome::Completed { .. }
    ));
    store
        .complete_runtime_task(&retry_token, json!({ "ok": true }), "2026-09-29T00:02:04Z")
        .expect("retry complete");

    assert!(matches!(
        store.complete_runtime_task(&token, json!({ "stale": true }), "2026-09-29T00:02:05Z"),
        Err(StoreError::RuntimeTaskConflict { .. })
    ));
    assert!(store.verify_runtime_state(&run_id).expect("verify").valid);
    assert_eq!(
        store
            .runtime_state(&run_id)
            .expect("state")
            .task(&lease.task_id)
            .expect("task")
            .status,
        TaskStatus::Succeeded
    );
}
