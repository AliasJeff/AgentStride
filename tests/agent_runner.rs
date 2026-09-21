use serde_json::json;

use agent_stride::{
    domain::Actor,
    runtime::{
        Command, CommandKind, RunId, TaskId,
        agent_worker::{
            AgentOutcome, AgentRunResult, AgentRunner, AgentSession, AgentWorker, AgentWorkerError,
            FakeWorker, FakeWorkerConfig, RetryPolicy,
        },
    },
    store::{Store, StoreError},
};

fn command(request_id: &str, kind: CommandKind) -> Command {
    Command::new(request_id, kind).expect("valid command")
}

struct ContextWorker;

impl AgentWorker for ContextWorker {
    fn execute(
        &mut self,
        lease: &agent_stride::store::RuntimeTaskLease,
        session: &mut AgentSession<'_>,
    ) -> Result<AgentOutcome, AgentWorkerError> {
        let context = session.context(lease)?;
        Ok(AgentOutcome::Completed {
            result: serde_json::to_value(context).expect("context JSON"),
        })
    }
}

#[test]
fn runner_exposes_plan_checkpoint_and_history_to_the_worker() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(ContextWorker, Actor::new("agent-a").expect("actor"));

    runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");

    let state = store.runtime_state(&run_id).expect("state");
    let task = state
        .task(&TaskId::new("task-a").expect("task"))
        .expect("task");
    assert_eq!(
        task.result.as_ref().expect("result")["plan_revision"],
        "plan-1"
    );
    assert_eq!(task.result.as_ref().expect("result")["task_id"], "task-a");
    assert_eq!(task.result.as_ref().expect("result")["attempt"], 1);
    assert!(task.result.as_ref().expect("result")["recent_events"].is_array());
}

fn setup() -> (Store, RunId) {
    let mut store = Store::open_in_memory().expect("open store");
    store.migrate().expect("migrate");
    let run_id = RunId::new("runner-run").expect("run");
    store
        .start_run(
            run_id.clone(),
            "plan-1",
            command(
                "start-runner-run",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ),
        )
        .expect("start");
    store
        .apply_runtime_command(
            &run_id,
            command(
                "reconcile-runner-run",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-1".to_owned(),
                    discovered: vec![TaskId::new("task-a").expect("task")],
                    ready: vec![TaskId::new("task-a").expect("task")],
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ),
        )
        .expect("reconcile");
    (store, run_id)
}

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Complete,
    HeartbeatThenComplete,
    TransientFailure,
    Checkpoint,
    Handoff,
    Wait,
    Crash,
}

struct ScriptedWorker {
    outcome: Outcome,
}

impl AgentWorker for ScriptedWorker {
    fn execute(
        &mut self,
        _lease: &agent_stride::store::RuntimeTaskLease,
        _session: &mut AgentSession<'_>,
    ) -> Result<AgentOutcome, AgentWorkerError> {
        Ok(match self.outcome {
            Outcome::Complete => AgentOutcome::Completed {
                result: json!({ "ok": true }),
            },
            Outcome::HeartbeatThenComplete => {
                _session
                    .heartbeat("2026-09-29T00:00:02Z", "2026-09-29T00:02:02Z")
                    .expect("heartbeat");
                AgentOutcome::Completed {
                    result: json!({ "heartbeat": true }),
                }
            }
            Outcome::TransientFailure => AgentOutcome::Failed {
                kind: agent_stride::runtime::agent_worker::ErrorKind::Transient,
                message: "upstream rate limit".to_owned(),
            },
            Outcome::Checkpoint => AgentOutcome::Checkpointed {
                progress: json!({ "step": 2, "total": 4 }),
            },
            Outcome::Handoff => AgentOutcome::HandedOff {
                to_worker: Some(Actor::new("agent-b").expect("actor")),
                summary: "research context is ready".to_owned(),
                artifacts: "notes.md".to_owned(),
                next_step: "validate source claims".to_owned(),
            },
            Outcome::Wait => AgentOutcome::WaitingForApproval,
            Outcome::Crash => AgentOutcome::Crashed { step: 3 },
        })
    }
}

#[test]
fn runner_commits_checkpoint_and_keeps_attempt_recoverable() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::Checkpoint,
        },
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");
    assert!(matches!(result, AgentRunResult::Checkpointed { .. }));

    let state = store.runtime_state(&run_id).expect("state");
    let task = state
        .task(&TaskId::new("task-a").expect("task"))
        .expect("task");
    assert_eq!(task.status, agent_stride::runtime::TaskStatus::Running);
    assert_eq!(task.checkpoint, Some(json!({ "step": 2, "total": 4 })));
    assert!(
        store
            .runtime_events(&run_id, 0, usize::MAX)
            .expect("events")
            .events
            .iter()
            .any(|event| matches!(
                event.kind,
                agent_stride::runtime::EventKind::TaskCheckpointed { .. }
            ))
    );

    let recovered = store
        .recover_expired_runtime_tasks(&run_id, "2026-09-29T00:01:03Z", 0)
        .expect("recover");
    assert_eq!(recovered.recovered.len(), 1);
}

#[test]
fn runner_resumes_checkpoints_within_one_attempt() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        FakeWorker::new(FakeWorkerConfig {
            seed: 11,
            failures_before_success: 0,
            crash_at_step: None,
            duration_steps: 3,
            heartbeat_every: None,
            proposal_id: None,
            handoff_to: None,
        }),
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_until_terminal(
            &mut store,
            "2026-09-29T00:00:02Z",
            "2026-09-29T00:01:02Z",
            4,
        )
        .expect("run");
    assert!(matches!(result, AgentRunResult::Completed { .. }));

    let state = store.runtime_state(&run_id).expect("state");
    let task = state
        .task(&TaskId::new("task-a").expect("task"))
        .expect("task");
    assert_eq!(task.status, agent_stride::runtime::TaskStatus::Succeeded);
    assert_eq!(
        task.checkpoint
            .as_ref()
            .and_then(|value| value["step"].as_u64()),
        Some(3)
    );
    let events = store
        .runtime_events(&run_id, 0, usize::MAX)
        .expect("events")
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.kind,
                agent_stride::runtime::EventKind::TaskClaimed { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.kind,
                agent_stride::runtime::EventKind::TaskCheckpointed { .. }
            ))
            .count(),
        3
    );
}

#[test]
fn runner_completes_a_task_through_the_store_boundary() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::Complete,
        },
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");
    assert!(matches!(result, AgentRunResult::Completed { .. }));
    assert_eq!(
        store
            .runtime_state(&run_id)
            .expect("state")
            .task(&TaskId::new("task-a").expect("task"))
            .expect("task")
            .status,
        agent_stride::runtime::TaskStatus::Succeeded
    );
}

#[test]
fn worker_session_heartbeats_without_polluting_history() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::HeartbeatThenComplete,
        },
        Actor::new("agent-a").expect("actor"),
    );

    runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");
    let events = store
        .runtime_events(&run_id, 0, usize::MAX)
        .expect("events")
        .events;
    assert!(
        !events
            .iter()
            .any(|event| event.event_id.contains("heartbeat"))
    );
    assert_eq!(
        store
            .runtime_state(&run_id)
            .expect("state")
            .task(&TaskId::new("task-a").expect("task"))
            .expect("task")
            .status,
        agent_stride::runtime::TaskStatus::Succeeded
    );
}

#[test]
fn transient_failure_becomes_history_and_persistent_retry() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::TransientFailure,
        },
        Actor::new("agent-a").expect("actor"),
    )
    .with_retry_policy(RetryPolicy {
        max_attempts: 3,
        initial_backoff_seconds: 5,
        max_backoff_seconds: 30,
    });

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");
    assert!(matches!(
        result,
        AgentRunResult::RetryScheduled { retry_at, .. }
            if retry_at == "2026-09-29T00:00:07Z"
    ));

    let state = store.runtime_state(&run_id).expect("state");
    assert_eq!(
        state
            .task(&TaskId::new("task-a").expect("task"))
            .expect("task")
            .status,
        agent_stride::runtime::TaskStatus::Waiting
    );
    assert!(
        store
            .runtime_events(&run_id, 0, usize::MAX)
            .expect("events")
            .events
            .iter()
            .any(|event| matches!(
                event.kind,
                agent_stride::runtime::EventKind::TaskRetryableFailed { .. }
            ))
    );

    let timer_id = state.timers.keys().next().cloned().expect("retry timer");
    store
        .fire_runtime_timer(timer_id, "2026-09-29T00:00:07Z")
        .expect("fire retry");
    let (retry, _) = store
        .poll_runtime_task(
            Actor::new("agent-b").expect("actor"),
            "2026-09-29T00:00:08Z",
            "2026-09-29T00:01:08Z",
        )
        .expect("poll retry");
    assert_eq!(retry.attempt, 2);
}

#[test]
fn runner_waiting_releases_lease_and_plan_can_resume_task() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::Wait,
        },
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("run");
    assert!(matches!(result, AgentRunResult::Waiting { .. }));
    assert_eq!(
        store
            .runtime_state(&run_id)
            .expect("state")
            .task(&TaskId::new("task-a").expect("task"))
            .expect("task")
            .status,
        agent_stride::runtime::TaskStatus::Waiting
    );

    store
        .apply_runtime_command(
            &run_id,
            command(
                "resume-task",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-1".to_owned(),
                    discovered: Vec::new(),
                    ready: vec![TaskId::new("task-a").expect("task")],
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:02:00Z".to_owned(),
                },
            ),
        )
        .expect("resume");
    let (lease, _) = store
        .poll_runtime_task(
            Actor::new("agent-b").expect("actor"),
            "2026-09-29T00:02:01Z",
            "2026-09-29T00:03:01Z",
        )
        .expect("poll resumed task");
    assert_eq!(lease.task_id.as_str(), "task-a");
}

#[test]
fn runner_handoff_requeues_task_for_the_target_worker() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::Handoff,
        },
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:02Z")
        .expect("handoff");
    assert!(matches!(
        result,
        AgentRunResult::HandedOff {
            to_worker: Some(ref worker),
            ..
        } if worker == "agent-b"
    ));

    assert!(matches!(
        store.poll_runtime_task(
            Actor::new("agent-c").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:01:03Z",
        ),
        Err(StoreError::NoRuntimeTask)
    ));
    let (lease, _) = store
        .poll_runtime_task(
            Actor::new("agent-b").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:01:03Z",
        )
        .expect("target worker receives handoff");
    assert_eq!(lease.worker.as_str(), "agent-b");
    assert_ne!(lease.attempt, 1);

    let events = store
        .runtime_events(&run_id, 0, usize::MAX)
        .expect("events")
        .events;
    assert!(events.iter().any(|event| matches!(
        event.kind,
        agent_stride::runtime::EventKind::TaskHandedOff { .. }
    )));
}

#[test]
fn runner_crash_leaves_no_completion_event() {
    let (mut store, run_id) = setup();
    let mut runner = AgentRunner::new(
        ScriptedWorker {
            outcome: Outcome::Crash,
        },
        Actor::new("agent-a").expect("actor"),
    );

    let result = runner
        .run_once(&mut store, "2026-09-29T00:00:02Z", "2026-09-29T00:01:00Z")
        .expect("run");
    assert!(matches!(result, AgentRunResult::Crashed { step: 3, .. }));
    let events = store
        .runtime_events(&run_id, 0, usize::MAX)
        .expect("events")
        .events;
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        agent_stride::runtime::EventKind::TaskSucceeded { .. }
            | agent_stride::runtime::EventKind::TaskFailed { .. }
    )));
    assert!(matches!(
        store.poll_runtime_task(
            Actor::new("agent-b").expect("actor"),
            "2026-09-29T00:00:03Z",
            "2026-09-29T00:01:03Z",
        ),
        Err(StoreError::NoRuntimeTask)
    ));
}
