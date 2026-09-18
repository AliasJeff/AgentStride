use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::domain::{Actor, DomainError};

pub mod agent_worker;
pub mod provider;
pub mod reconciler;
pub mod token;

mod ids {
    use std::fmt;

    use serde::{Deserialize, Serialize};

    use crate::runtime::RuntimeError;

    macro_rules! runtime_id {
        ($name:ident) => {
            #[derive(
                Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            )]
            #[serde(transparent)]
            pub struct $name(String);

            impl $name {
                pub fn new(value: impl Into<String>) -> Result<Self, RuntimeError> {
                    let value = value.into();
                    if value.trim().is_empty() {
                        return Err(RuntimeError::EmptyIdentifier {
                            kind: stringify!($name),
                        });
                    }
                    Ok(Self(value))
                }

                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str(&self.0)
                }
            }
        };
    }

    runtime_id!(RunId);
    runtime_id!(TaskId);
    runtime_id!(AttemptId);
    runtime_id!(TimerId);
}

pub use ids::{AttemptId, RunId, TaskId, TimerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created,
    Running,
    Paused,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Ready,
    Scheduled,
    Claimed,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTask {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub attempt: u32,
    pub active_attempt: Option<AttemptId>,
    pub worker: Option<Actor>,
    pub checkpoint: Option<Value>,
    pub result: Option<Value>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSupersession {
    pub task_id: TaskId,
    pub replacement_task_id: TaskId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerState {
    pub timer_id: TimerId,
    pub task_id: TaskId,
    pub fire_at: String,
    pub fired: bool,
}

impl RunTask {
    fn discovered(task_id: TaskId) -> Self {
        Self {
            task_id,
            status: TaskStatus::Pending,
            attempt: 0,
            active_attempt: None,
            worker: None,
            checkpoint: None,
            result: None,
            failure: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutableState {
    pub run_id: RunId,
    pub plan_revision: String,
    pub status: RunStatus,
    pub history_version: u64,
    pub tasks: BTreeMap<TaskId, RunTask>,
    pub timers: BTreeMap<TimerId, TimerState>,
}

impl MutableState {
    pub fn new(run_id: RunId, plan_revision: impl Into<String>) -> Result<Self, RuntimeError> {
        let plan_revision = plan_revision.into();
        if plan_revision.trim().is_empty() {
            return Err(RuntimeError::EmptyPlanRevision);
        }
        Ok(Self {
            run_id,
            plan_revision,
            status: RunStatus::Created,
            history_version: 0,
            tasks: BTreeMap::new(),
            timers: BTreeMap::new(),
        })
    }

    pub fn handle(&mut self, command: Command) -> Result<Vec<Event>, RuntimeError> {
        let events = decide(self, command)?;
        for event in &events {
            apply(self, event)?;
        }
        Ok(events)
    }

    pub fn task(&self, task_id: &TaskId) -> Result<&RunTask, RuntimeError> {
        self.tasks
            .get(task_id)
            .ok_or_else(|| RuntimeError::TaskNotFound {
                task_id: task_id.to_string(),
            })
    }

    pub fn reconcile_plan(
        &self,
        new_plan_revision: impl Into<String>,
        discovered: Vec<TaskId>,
        ready: Vec<TaskId>,
        invalidated: Vec<TaskId>,
        superseded: Vec<TaskSupersession>,
        now: impl Into<String>,
    ) -> Result<Command, RuntimeError> {
        reconciler::command(
            self,
            new_plan_revision,
            discovered,
            ready,
            invalidated,
            superseded,
            now,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub request_id: String,
    pub kind: CommandKind,
}

impl Command {
    pub fn new(request_id: impl Into<String>, kind: CommandKind) -> Result<Self, RuntimeError> {
        let request_id = request_id.into();
        if request_id.trim().is_empty() {
            return Err(RuntimeError::EmptyRequestId);
        }
        Ok(Self { request_id, kind })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandKind {
    StartRun {
        now: String,
    },
    DiscoverTask {
        task_id: TaskId,
        now: String,
    },
    ScheduleTask {
        task_id: TaskId,
        now: String,
    },
    ClaimTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        now: String,
    },
    StartAttempt {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        now: String,
    },
    CheckpointTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        progress: Value,
        now: String,
    },
    CompleteTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        result: Value,
        now: String,
    },
    WaitTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        reason: String,
        now: String,
    },
    HandoffTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        to_worker: Option<Actor>,
        summary: String,
        artifacts: String,
        next_step: String,
        now: String,
    },
    RetryTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        timer_id: TimerId,
        retry_at: String,
        error: String,
        now: String,
    },
    FailTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        error: String,
        now: String,
    },
    TimeoutTask {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        timer_id: TimerId,
        retry_at: String,
        now: String,
    },
    FireTimer {
        timer_id: TimerId,
        now: String,
    },
    PauseRun {
        now: String,
    },
    ResumeRun {
        now: String,
    },
    ReconcilePlan {
        new_plan_revision: String,
        discovered: Vec<TaskId>,
        ready: Vec<TaskId>,
        invalidated: Vec<TaskId>,
        superseded: Vec<TaskSupersession>,
        now: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub run_id: RunId,
    pub seq: u64,
    pub event_id: String,
    pub occurred_at: String,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    RunStarted {
        plan_revision: String,
    },
    TaskDiscovered {
        task_id: TaskId,
    },
    TaskReady {
        task_id: TaskId,
    },
    TaskScheduled {
        task_id: TaskId,
    },
    TaskClaimed {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
    },
    TaskStarted {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
    },
    TaskCheckpointed {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        progress: Value,
    },
    TaskSucceeded {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        result: Value,
    },
    TaskWaiting {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        reason: String,
    },
    TaskHandedOff {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        to_worker: Option<Actor>,
        summary: String,
        artifacts: String,
        next_step: String,
    },
    TaskRetryableFailed {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        error: String,
    },
    TaskFailed {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        error: String,
    },
    TaskTimedOut {
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
    },
    RetryScheduled {
        task_id: TaskId,
        attempt_id: AttemptId,
        timer_id: TimerId,
        retry_at: String,
    },
    TimerCreated {
        timer_id: TimerId,
        task_id: TaskId,
        fire_at: String,
    },
    TimerFired {
        timer_id: TimerId,
    },
    RunPaused,
    RunResumed,
    PlanRevisionActivated {
        old_plan_revision: String,
        new_plan_revision: String,
    },
    TaskInvalidated {
        task_id: TaskId,
        reason: String,
    },
    TaskSuperseded {
        task_id: TaskId,
        replacement_task_id: TaskId,
        reason: String,
    },
}

pub fn decide(state: &MutableState, command: Command) -> Result<Vec<Event>, RuntimeError> {
    let (occurred_at, kinds) = decide_kinds(state, &command.kind)?;
    kinds
        .into_iter()
        .enumerate()
        .map(|(offset, kind)| {
            let seq = state
                .history_version
                .checked_add(offset as u64 + 1)
                .ok_or(RuntimeError::HistoryVersionOverflow)?;
            Ok(Event {
                run_id: state.run_id.clone(),
                seq,
                event_id: if offset == 0 {
                    command.request_id.clone()
                } else {
                    format!("{}-{offset}", command.request_id)
                },
                occurred_at: occurred_at.clone(),
                kind,
            })
        })
        .collect()
}

pub fn apply(state: &mut MutableState, event: &Event) -> Result<(), RuntimeError> {
    if event.run_id != state.run_id {
        return Err(RuntimeError::RunMismatch {
            expected: state.run_id.to_string(),
            actual: event.run_id.to_string(),
        });
    }
    let expected_seq = state.history_version.saturating_add(1);
    if event.seq != expected_seq {
        return Err(RuntimeError::EventSequence {
            expected: expected_seq,
            actual: event.seq,
        });
    }
    if event.occurred_at.trim().is_empty() {
        return Err(RuntimeError::EmptyTimestamp);
    }

    apply_kind(state, &event.kind)?;
    state.history_version = event.seq;
    Ok(())
}

fn decide_kinds(
    state: &MutableState,
    command: &CommandKind,
) -> Result<(String, Vec<EventKind>), RuntimeError> {
    match command {
        CommandKind::StartRun { now } => {
            require_timestamp(now)?;
            require_run_status(state, RunStatus::Created)?;
            Ok((
                now.clone(),
                vec![EventKind::RunStarted {
                    plan_revision: state.plan_revision.clone(),
                }],
            ))
        }
        CommandKind::DiscoverTask { task_id, now } => {
            require_timestamp(now)?;
            require_running(state)?;
            if state.tasks.contains_key(task_id) {
                return Err(RuntimeError::TaskAlreadyExists {
                    task_id: task_id.to_string(),
                });
            }
            Ok((
                now.clone(),
                vec![
                    EventKind::TaskDiscovered {
                        task_id: task_id.clone(),
                    },
                    EventKind::TaskReady {
                        task_id: task_id.clone(),
                    },
                ],
            ))
        }
        CommandKind::ScheduleTask { task_id, now } => {
            require_timestamp(now)?;
            require_running(state)?;
            require_task_status(state, task_id, TaskStatus::Ready)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskScheduled {
                    task_id: task_id.clone(),
                }],
            ))
        }
        CommandKind::ClaimTask {
            task_id,
            attempt_id,
            worker,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            require_task_status(state, task_id, TaskStatus::Scheduled)?;
            if worker.as_str().trim().is_empty() {
                return Err(DomainError::InvalidActor.into());
            }
            if attempt_id.as_str().trim().is_empty() {
                return Err(RuntimeError::EmptyIdentifier { kind: "AttemptId" });
            }
            Ok((
                now.clone(),
                vec![EventKind::TaskClaimed {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                }],
            ))
        }
        CommandKind::StartAttempt {
            task_id,
            attempt_id,
            worker,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Claimed)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskStarted {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                }],
            ))
        }
        CommandKind::CheckpointTask {
            task_id,
            attempt_id,
            worker,
            progress,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Running)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskCheckpointed {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                    progress: progress.clone(),
                }],
            ))
        }
        CommandKind::CompleteTask {
            task_id,
            attempt_id,
            worker,
            result,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Running)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskSucceeded {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                    result: result.clone(),
                }],
            ))
        }
        CommandKind::WaitTask {
            task_id,
            attempt_id,
            worker,
            reason,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            if reason.trim().is_empty() {
                return Err(RuntimeError::EmptyFailure);
            }
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskWaiting {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                    reason: reason.clone(),
                }],
            ))
        }
        CommandKind::RetryTask {
            task_id,
            attempt_id,
            worker,
            timer_id,
            retry_at,
            error,
            now,
        } => {
            require_timestamp(now)?;
            require_timestamp(retry_at)?;
            require_running(state)?;
            if error.trim().is_empty() {
                return Err(RuntimeError::EmptyFailure);
            }
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            if state.timers.contains_key(timer_id) {
                return Err(RuntimeError::TimerAlreadyExists {
                    timer_id: timer_id.to_string(),
                });
            }
            Ok((
                now.clone(),
                vec![
                    EventKind::TaskRetryableFailed {
                        task_id: task_id.clone(),
                        attempt_id: attempt_id.clone(),
                        worker: worker.clone(),
                        error: error.clone(),
                    },
                    EventKind::RetryScheduled {
                        task_id: task_id.clone(),
                        attempt_id: attempt_id.clone(),
                        timer_id: timer_id.clone(),
                        retry_at: retry_at.clone(),
                    },
                    EventKind::TimerCreated {
                        timer_id: timer_id.clone(),
                        task_id: task_id.clone(),
                        fire_at: retry_at.clone(),
                    },
                ],
            ))
        }
        CommandKind::HandoffTask {
            task_id,
            attempt_id,
            worker,
            to_worker,
            summary,
            artifacts,
            next_step,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            if summary.trim().is_empty() || next_step.trim().is_empty() {
                return Err(RuntimeError::EmptyFailure);
            }
            if to_worker.as_ref() == Some(worker) {
                return Err(RuntimeError::WorkerMismatch {
                    expected: Some("a different worker".to_owned()),
                    actual: worker.as_str().to_owned(),
                });
            }
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskHandedOff {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                    to_worker: to_worker.clone(),
                    summary: summary.clone(),
                    artifacts: artifacts.clone(),
                    next_step: next_step.clone(),
                }],
            ))
        }
        CommandKind::FailTask {
            task_id,
            attempt_id,
            worker,
            error,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            if error.trim().is_empty() {
                return Err(RuntimeError::EmptyFailure);
            }
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Running)?;
            Ok((
                now.clone(),
                vec![EventKind::TaskFailed {
                    task_id: task_id.clone(),
                    attempt_id: attempt_id.clone(),
                    worker: worker.clone(),
                    error: error.clone(),
                }],
            ))
        }
        CommandKind::TimeoutTask {
            task_id,
            attempt_id,
            worker,
            timer_id,
            retry_at,
            now,
        } => {
            require_timestamp(now)?;
            require_timestamp(retry_at)?;
            require_running(state)?;
            let task = state.task(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            if state.timers.contains_key(timer_id) {
                return Err(RuntimeError::TimerAlreadyExists {
                    timer_id: timer_id.to_string(),
                });
            }
            Ok((
                now.clone(),
                vec![
                    EventKind::TaskTimedOut {
                        task_id: task_id.clone(),
                        attempt_id: attempt_id.clone(),
                        worker: worker.clone(),
                    },
                    EventKind::RetryScheduled {
                        task_id: task_id.clone(),
                        attempt_id: attempt_id.clone(),
                        timer_id: timer_id.clone(),
                        retry_at: retry_at.clone(),
                    },
                    EventKind::TimerCreated {
                        timer_id: timer_id.clone(),
                        task_id: task_id.clone(),
                        fire_at: retry_at.clone(),
                    },
                ],
            ))
        }
        CommandKind::FireTimer { timer_id, now } => {
            require_timestamp(now)?;
            require_running(state)?;
            let timer = state
                .timers
                .get(timer_id)
                .ok_or_else(|| RuntimeError::TimerNotFound {
                    timer_id: timer_id.to_string(),
                })?;
            if timer.fired {
                return Err(RuntimeError::TimerAlreadyFired {
                    timer_id: timer_id.to_string(),
                });
            }
            if timer.fire_at.as_str() > now.as_str() {
                return Err(RuntimeError::TimerNotDue {
                    timer_id: timer_id.to_string(),
                });
            }
            Ok((
                now.clone(),
                vec![
                    EventKind::TimerFired {
                        timer_id: timer_id.clone(),
                    },
                    EventKind::TaskReady {
                        task_id: timer.task_id.clone(),
                    },
                    EventKind::TaskScheduled {
                        task_id: timer.task_id.clone(),
                    },
                ],
            ))
        }
        CommandKind::PauseRun { now } => {
            require_timestamp(now)?;
            require_run_status(state, RunStatus::Running)?;
            Ok((now.clone(), vec![EventKind::RunPaused]))
        }
        CommandKind::ResumeRun { now } => {
            require_timestamp(now)?;
            require_run_status(state, RunStatus::Paused)?;
            Ok((now.clone(), vec![EventKind::RunResumed]))
        }
        CommandKind::ReconcilePlan {
            new_plan_revision,
            discovered,
            ready,
            invalidated,
            superseded,
            now,
        } => {
            require_timestamp(now)?;
            require_running(state)?;
            if new_plan_revision.trim().is_empty() {
                return Err(RuntimeError::EmptyPlanRevision);
            }
            let mut events = Vec::new();
            if new_plan_revision != &state.plan_revision {
                events.push(EventKind::PlanRevisionActivated {
                    old_plan_revision: state.plan_revision.clone(),
                    new_plan_revision: new_plan_revision.clone(),
                });
            }
            let mut seen = std::collections::BTreeSet::new();
            let discovered_set = discovered
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();

            for task_id in invalidated {
                validate_plan_task_id(state, task_id, &mut seen)?;
                validate_invalidatable(state, task_id)?;
                events.push(EventKind::TaskInvalidated {
                    task_id: task_id.clone(),
                    reason: "removed from plan revision".to_owned(),
                });
            }
            let mut replacement_ids = std::collections::BTreeSet::new();
            for change in superseded {
                validate_plan_task_id(state, &change.task_id, &mut seen)?;
                if change.reason.trim().is_empty() {
                    return Err(RuntimeError::EmptyPlanChangeReason);
                }
                if change.task_id == change.replacement_task_id {
                    return Err(RuntimeError::TaskSupersessionCycle {
                        task_id: change.task_id.to_string(),
                    });
                }
                if !replacement_ids.insert(change.replacement_task_id.clone())
                    || state.tasks.contains_key(&change.replacement_task_id)
                {
                    return Err(RuntimeError::TaskAlreadyExists {
                        task_id: change.replacement_task_id.to_string(),
                    });
                }
                validate_invalidatable(state, &change.task_id)?;
                events.push(EventKind::TaskSuperseded {
                    task_id: change.task_id.clone(),
                    replacement_task_id: change.replacement_task_id.clone(),
                    reason: change.reason.clone(),
                });
            }
            for task_id in discovered {
                if !seen.insert(task_id.clone()) {
                    return Err(RuntimeError::DuplicatePlanTask {
                        task_id: task_id.to_string(),
                    });
                }
                if state.tasks.contains_key(task_id) {
                    return Err(RuntimeError::TaskAlreadyExists {
                        task_id: task_id.to_string(),
                    });
                }
                events.push(EventKind::TaskDiscovered {
                    task_id: task_id.clone(),
                });
            }
            let mut ready_seen = std::collections::BTreeSet::new();
            for task_id in ready {
                if !ready_seen.insert(task_id.clone()) {
                    return Err(RuntimeError::DuplicatePlanTask {
                        task_id: task_id.to_string(),
                    });
                }
                if !discovered_set.contains(task_id) {
                    let task = state.task(task_id)?;
                    if !matches!(task.status, TaskStatus::Pending | TaskStatus::Waiting) {
                        return Err(RuntimeError::InvalidTaskTransition {
                            task_id: task_id.to_string(),
                            from: task.status,
                            to: TaskStatus::Ready,
                        });
                    }
                }
                events.push(EventKind::TaskReady {
                    task_id: task_id.clone(),
                });
                events.push(EventKind::TaskScheduled {
                    task_id: task_id.clone(),
                });
            }
            Ok((now.clone(), events))
        }
    }
}

fn apply_kind(state: &mut MutableState, event: &EventKind) -> Result<(), RuntimeError> {
    match event {
        EventKind::RunStarted { plan_revision } => {
            require_run_status(state, RunStatus::Created)?;
            if plan_revision != &state.plan_revision {
                return Err(RuntimeError::PlanRevisionMismatch {
                    expected: state.plan_revision.clone(),
                    actual: plan_revision.clone(),
                });
            }
            state.status = RunStatus::Running;
        }
        EventKind::TaskDiscovered { task_id } => {
            require_running(state)?;
            if state.tasks.contains_key(task_id) {
                return Err(RuntimeError::TaskAlreadyExists {
                    task_id: task_id.to_string(),
                });
            }
            state
                .tasks
                .insert(task_id.clone(), RunTask::discovered(task_id.clone()));
        }
        EventKind::TaskReady { task_id } => {
            let task = state.task_mut(task_id)?;
            if !matches!(task.status, TaskStatus::Pending | TaskStatus::Waiting) {
                return Err(RuntimeError::InvalidTaskTransition {
                    task_id: task.task_id.to_string(),
                    from: task.status,
                    to: TaskStatus::Ready,
                });
            }
            task.status = TaskStatus::Ready;
        }
        EventKind::TaskScheduled { task_id } => {
            let task = state.task_mut(task_id)?;
            require_status(task, TaskStatus::Ready)?;
            task.status = TaskStatus::Scheduled;
        }
        EventKind::TaskClaimed {
            task_id,
            attempt_id,
            worker,
        } => {
            let task = state.task_mut(task_id)?;
            require_status(task, TaskStatus::Scheduled)?;
            task.attempt = task.attempt.saturating_add(1);
            task.active_attempt = Some(attempt_id.clone());
            task.worker = Some(worker.clone());
            task.status = TaskStatus::Claimed;
        }
        EventKind::TaskStarted {
            task_id,
            attempt_id,
            worker,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Claimed)?;
            task.status = TaskStatus::Running;
        }
        EventKind::TaskCheckpointed {
            task_id,
            attempt_id,
            worker,
            progress,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_status(task, TaskStatus::Running)?;
            task.checkpoint = Some(progress.clone());
        }
        EventKind::TaskSucceeded {
            task_id,
            attempt_id,
            worker,
            result,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Succeeded;
            task.result = Some(result.clone());
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::TaskWaiting {
            task_id,
            attempt_id,
            worker,
            reason,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Waiting;
            task.failure = Some(reason.clone());
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::TaskHandedOff {
            task_id,
            attempt_id,
            worker,
            ..
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Scheduled;
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::TaskRetryableFailed {
            task_id,
            attempt_id,
            worker,
            error,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Waiting;
            task.failure = Some(error.clone());
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::TaskFailed {
            task_id,
            attempt_id,
            worker,
            error,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Failed;
            task.failure = Some(error.clone());
            task.active_attempt = None;
        }
        EventKind::TaskTimedOut {
            task_id,
            attempt_id,
            worker,
        } => {
            let task = state.task_mut(task_id)?;
            require_attempt(task, attempt_id, worker)?;
            require_active_attempt_status(task)?;
            task.status = TaskStatus::Waiting;
            task.failure = Some("task timed out".to_owned());
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::RetryScheduled {
            task_id,
            attempt_id,
            timer_id,
            retry_at,
        } => {
            let task = state.task(task_id)?;
            require_status(task, TaskStatus::Waiting)?;
            if task.active_attempt.is_some() || retry_at.trim().is_empty() {
                return Err(RuntimeError::InvalidRetrySchedule {
                    task_id: task_id.to_string(),
                });
            }
            if state.timers.contains_key(timer_id) {
                return Err(RuntimeError::TimerAlreadyExists {
                    timer_id: timer_id.to_string(),
                });
            }
            if attempt_id.as_str().trim().is_empty() {
                return Err(RuntimeError::EmptyIdentifier { kind: "AttemptId" });
            }
        }
        EventKind::TimerCreated {
            timer_id,
            task_id,
            fire_at,
        } => {
            if state.timers.contains_key(timer_id) {
                return Err(RuntimeError::TimerAlreadyExists {
                    timer_id: timer_id.to_string(),
                });
            }
            if state.task(task_id)?.status != TaskStatus::Waiting {
                return Err(RuntimeError::InvalidRetrySchedule {
                    task_id: task_id.to_string(),
                });
            }
            require_timestamp(fire_at)?;
            state.timers.insert(
                timer_id.clone(),
                TimerState {
                    timer_id: timer_id.clone(),
                    task_id: task_id.clone(),
                    fire_at: fire_at.clone(),
                    fired: false,
                },
            );
        }
        EventKind::TimerFired { timer_id } => {
            let timer =
                state
                    .timers
                    .get_mut(timer_id)
                    .ok_or_else(|| RuntimeError::TimerNotFound {
                        timer_id: timer_id.to_string(),
                    })?;
            if timer.fired {
                return Err(RuntimeError::TimerAlreadyFired {
                    timer_id: timer_id.to_string(),
                });
            }
            timer.fired = true;
        }
        EventKind::RunPaused => {
            require_run_status(state, RunStatus::Running)?;
            state.status = RunStatus::Paused;
        }
        EventKind::RunResumed => {
            require_run_status(state, RunStatus::Paused)?;
            state.status = RunStatus::Running;
        }
        EventKind::PlanRevisionActivated {
            old_plan_revision,
            new_plan_revision,
        } => {
            require_running(state)?;
            if old_plan_revision != &state.plan_revision {
                return Err(RuntimeError::PlanRevisionMismatch {
                    expected: state.plan_revision.clone(),
                    actual: old_plan_revision.clone(),
                });
            }
            if new_plan_revision.trim().is_empty() {
                return Err(RuntimeError::EmptyPlanRevision);
            }
            state.plan_revision = new_plan_revision.clone();
        }
        EventKind::TaskInvalidated { task_id, reason } => {
            if reason.trim().is_empty() {
                return Err(RuntimeError::EmptyPlanChangeReason);
            }
            let task = state.task_mut(task_id)?;
            validate_invalidatable_task(task)?;
            task.status = TaskStatus::Invalidated;
            task.failure = Some(reason.clone());
            task.active_attempt = None;
            task.worker = None;
        }
        EventKind::TaskSuperseded {
            task_id,
            replacement_task_id,
            reason,
        } => {
            if reason.trim().is_empty() {
                return Err(RuntimeError::EmptyPlanChangeReason);
            }
            if task_id == replacement_task_id {
                return Err(RuntimeError::TaskSupersessionCycle {
                    task_id: task_id.to_string(),
                });
            }
            let task = state.task_mut(task_id)?;
            validate_invalidatable_task(task)?;
            task.status = TaskStatus::Invalidated;
            task.failure = Some(format!("superseded by {replacement_task_id}: {reason}"));
            task.active_attempt = None;
            task.worker = None;
        }
    }
    Ok(())
}

fn validate_plan_task_id(
    state: &MutableState,
    task_id: &TaskId,
    seen: &mut std::collections::BTreeSet<TaskId>,
) -> Result<(), RuntimeError> {
    if !seen.insert(task_id.clone()) {
        return Err(RuntimeError::DuplicatePlanTask {
            task_id: task_id.to_string(),
        });
    }
    if !state.tasks.contains_key(task_id) {
        return Err(RuntimeError::TaskNotFound {
            task_id: task_id.to_string(),
        });
    }
    Ok(())
}

fn validate_invalidatable(state: &MutableState, task_id: &TaskId) -> Result<(), RuntimeError> {
    validate_invalidatable_task(state.task(task_id)?)
}

fn validate_invalidatable_task(task: &RunTask) -> Result<(), RuntimeError> {
    if matches!(
        task.status,
        TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Invalidated
    ) {
        return Err(RuntimeError::TaskAlreadyTerminal {
            task_id: task.task_id.to_string(),
            status: task.status,
        });
    }
    Ok(())
}

fn task_mut<'a>(
    state: &'a mut MutableState,
    task_id: &TaskId,
) -> Result<&'a mut RunTask, RuntimeError> {
    state
        .tasks
        .get_mut(task_id)
        .ok_or_else(|| RuntimeError::TaskNotFound {
            task_id: task_id.to_string(),
        })
}

impl MutableState {
    fn task_mut(&mut self, task_id: &TaskId) -> Result<&mut RunTask, RuntimeError> {
        task_mut(self, task_id)
    }
}

fn require_timestamp(value: &str) -> Result<(), RuntimeError> {
    if value.trim().is_empty() {
        Err(RuntimeError::EmptyTimestamp)
    } else {
        Ok(())
    }
}

fn require_running(state: &MutableState) -> Result<(), RuntimeError> {
    require_run_status(state, RunStatus::Running)
}

fn require_run_status(state: &MutableState, expected: RunStatus) -> Result<(), RuntimeError> {
    if state.status == expected {
        Ok(())
    } else {
        Err(RuntimeError::InvalidRunTransition {
            from: state.status,
            to: expected,
        })
    }
}

fn require_task_status(
    state: &MutableState,
    task_id: &TaskId,
    expected: TaskStatus,
) -> Result<(), RuntimeError> {
    let task = state.task(task_id)?;
    require_status(task, expected)
}

fn require_status(task: &RunTask, expected: TaskStatus) -> Result<(), RuntimeError> {
    if task.status == expected {
        Ok(())
    } else {
        Err(RuntimeError::InvalidTaskTransition {
            task_id: task.task_id.to_string(),
            from: task.status,
            to: expected,
        })
    }
}

fn require_active_attempt_status(task: &RunTask) -> Result<(), RuntimeError> {
    if matches!(task.status, TaskStatus::Claimed | TaskStatus::Running) {
        Ok(())
    } else {
        Err(RuntimeError::InvalidTaskTransition {
            task_id: task.task_id.to_string(),
            from: task.status,
            to: TaskStatus::Running,
        })
    }
}

fn require_attempt(
    task: &RunTask,
    attempt_id: &AttemptId,
    worker: &Actor,
) -> Result<(), RuntimeError> {
    if task.active_attempt.as_ref() != Some(attempt_id) {
        return Err(RuntimeError::AttemptMismatch {
            expected: task.active_attempt.as_ref().map(ToString::to_string),
            actual: attempt_id.to_string(),
        });
    }
    if task.worker.as_ref() != Some(worker) {
        return Err(RuntimeError::WorkerMismatch {
            expected: task.worker.as_ref().map(|value| value.as_str().to_owned()),
            actual: worker.as_str().to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq)]
pub enum RuntimeError {
    #[error("runtime identifier `{kind}` must not be empty")]
    EmptyIdentifier { kind: &'static str },

    #[error("request_id must not be empty")]
    EmptyRequestId,

    #[error("plan revision must not be empty")]
    EmptyPlanRevision,

    #[error("timestamp must not be empty")]
    EmptyTimestamp,

    #[error("task failure must not be empty")]
    EmptyFailure,

    #[error("run `{expected}` does not match event run `{actual}`")]
    RunMismatch { expected: String, actual: String },

    #[error("event sequence mismatch: expected {expected}, actual {actual}")]
    EventSequence { expected: u64, actual: u64 },

    #[error("runtime history version overflowed")]
    HistoryVersionOverflow,

    #[error("invalid timestamp format `{value}`")]
    InvalidTimestampFormat { value: String },

    #[error("retry delay must not be negative: {seconds}")]
    InvalidRetryDelay { seconds: i64 },

    #[error("timer `{timer_id}` already exists")]
    TimerAlreadyExists { timer_id: String },

    #[error("timer `{timer_id}` was not found")]
    TimerNotFound { timer_id: String },

    #[error("timer `{timer_id}` is not due")]
    TimerNotDue { timer_id: String },

    #[error("timer `{timer_id}` has already fired")]
    TimerAlreadyFired { timer_id: String },

    #[error("invalid retry schedule for task `{task_id}`")]
    InvalidRetrySchedule { task_id: String },

    #[error("task `{task_id}` does not exist")]
    TaskNotFound { task_id: String },

    #[error("task `{task_id}` already exists")]
    TaskAlreadyExists { task_id: String },

    #[error("plan revision mismatch: expected `{expected}`, actual `{actual}`")]
    PlanRevisionMismatch { expected: String, actual: String },

    #[error("plan change reason must not be empty")]
    EmptyPlanChangeReason,

    #[error("task `{task_id}` appears more than once in a plan change")]
    DuplicatePlanTask { task_id: String },

    #[error("task `{task_id}` is already terminal with status `{status:?}`")]
    TaskAlreadyTerminal { task_id: String, status: TaskStatus },

    #[error("task `{task_id}` cannot supersede itself")]
    TaskSupersessionCycle { task_id: String },

    #[error("invalid run transition from `{from:?}`; expected `{to:?}`")]
    InvalidRunTransition { from: RunStatus, to: RunStatus },

    #[error("invalid task `{task_id}` transition from `{from:?}`; expected `{to:?}`")]
    InvalidTaskTransition {
        task_id: String,
        from: TaskStatus,
        to: TaskStatus,
    },

    #[error("attempt mismatch: expected `{expected:?}`, actual `{actual}`")]
    AttemptMismatch {
        expected: Option<String>,
        actual: String,
    },

    #[error("lease for task `{task_id}` expired at `{lease_until}` (now `{now}`)")]
    LeaseExpired {
        task_id: String,
        lease_until: String,
        now: String,
    },

    #[error("worker mismatch: expected `{expected:?}`, actual `{actual}`")]
    WorkerMismatch {
        expected: Option<String>,
        actual: String,
    },

    #[error("invalid event type: expected `{expected}`, actual `{actual}`")]
    InvalidEventType { expected: String, actual: String },

    #[error(transparent)]
    Domain(#[from] DomainError),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AttemptId, Command, CommandKind, Event, EventKind, MutableState, RunId, RunStatus,
        RuntimeError, TaskId, TaskStatus, TaskSupersession, apply,
    };
    use crate::domain::Actor;

    fn run() -> MutableState {
        MutableState::new(RunId::new("run-1").expect("run id"), "plan-1").expect("state")
    }

    fn command(request_id: &str, kind: CommandKind) -> Command {
        Command::new(request_id, kind).expect("command")
    }

    fn id(value: &str) -> TaskId {
        TaskId::new(value).expect("task id")
    }

    fn attempt(value: &str) -> AttemptId {
        AttemptId::new(value).expect("attempt id")
    }

    fn actor(value: &str) -> Actor {
        Actor::new(value).expect("actor")
    }

    fn start_and_schedule(state: &mut MutableState) {
        state
            .handle(command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ))
            .expect("start");
        state
            .handle(command(
                "discover",
                CommandKind::DiscoverTask {
                    task_id: id("task-a"),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ))
            .expect("discover");
        state
            .handle(command(
                "schedule",
                CommandKind::ScheduleTask {
                    task_id: id("task-a"),
                    now: "2026-09-29T00:00:02Z".to_owned(),
                },
            ))
            .expect("schedule");
    }

    #[test]
    fn command_event_reducer_advances_a_task() {
        let mut state = run();
        start_and_schedule(&mut state);

        let events = state
            .handle(command(
                "claim",
                CommandKind::ClaimTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:03Z".to_owned(),
                },
            ))
            .expect("claim");
        assert_eq!(events[0].seq, 5);

        state
            .handle(command(
                "start-attempt",
                CommandKind::StartAttempt {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:04Z".to_owned(),
                },
            ))
            .expect("start attempt");
        state
            .handle(command(
                "complete",
                CommandKind::CompleteTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    result: json!({ "ok": true }),
                    now: "2026-09-29T00:00:05Z".to_owned(),
                },
            ))
            .expect("complete");

        let task = state.task(&id("task-a")).expect("task");
        assert_eq!(task.status, TaskStatus::Succeeded);
        assert_eq!(task.result, Some(json!({ "ok": true })));
        assert_eq!(state.history_version, 7);
    }

    #[test]
    fn discover_is_atomic_from_the_state_machine_perspective() {
        let mut state = run();
        state
            .handle(command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ))
            .expect("start");
        let events = state
            .handle(command(
                "discover",
                CommandKind::DiscoverTask {
                    task_id: id("task-a"),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ))
            .expect("discover");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
        assert_eq!(events[1].seq, 3);
        assert_eq!(
            state.task(&id("task-a")).expect("task").status,
            TaskStatus::Ready
        );
    }

    #[test]
    fn stale_attempt_cannot_complete_a_task() {
        let mut state = run();
        start_and_schedule(&mut state);
        state
            .handle(command(
                "claim",
                CommandKind::ClaimTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:03Z".to_owned(),
                },
            ))
            .expect("claim");
        state
            .handle(command(
                "start-attempt",
                CommandKind::StartAttempt {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:04Z".to_owned(),
                },
            ))
            .expect("start attempt");

        let error = state
            .handle(command(
                "stale-complete",
                CommandKind::CompleteTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-old"),
                    worker: actor("worker-a"),
                    result: json!({}),
                    now: "2026-09-29T00:00:05Z".to_owned(),
                },
            ))
            .expect_err("stale attempt must fail");
        assert!(matches!(error, RuntimeError::AttemptMismatch { .. }));
        assert_eq!(
            state.task(&id("task-a")).expect("task").status,
            TaskStatus::Running
        );
    }

    #[test]
    fn event_replay_reconstructs_the_same_state() {
        let mut original = run();
        let mut history = Vec::new();
        for command in [
            command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ),
            command(
                "discover",
                CommandKind::DiscoverTask {
                    task_id: id("task-a"),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ),
            command(
                "schedule",
                CommandKind::ScheduleTask {
                    task_id: id("task-a"),
                    now: "2026-09-29T00:00:02Z".to_owned(),
                },
            ),
        ] {
            history.extend(original.handle(command).expect("command"));
        }

        let mut rebuilt = run();
        for event in &history {
            apply(&mut rebuilt, event).expect("replay event");
        }
        assert_eq!(original, rebuilt);
    }

    #[test]
    fn event_sequence_must_be_contiguous() {
        let mut state = run();
        let event = Event {
            run_id: state.run_id.clone(),
            seq: 2,
            event_id: "start".to_owned(),
            occurred_at: "2026-09-29T00:00:00Z".to_owned(),
            kind: super::EventKind::RunStarted {
                plan_revision: "plan-1".to_owned(),
            },
        };
        assert!(matches!(
            apply(&mut state, &event),
            Err(RuntimeError::EventSequence {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn run_pause_and_resume_are_explicit_transitions() {
        let mut state = run();
        state
            .handle(command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ))
            .expect("start");
        state
            .handle(command(
                "pause",
                CommandKind::PauseRun {
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ))
            .expect("pause");
        assert_eq!(state.status, RunStatus::Paused);
        state
            .handle(command(
                "resume",
                CommandKind::ResumeRun {
                    now: "2026-09-29T00:00:02Z".to_owned(),
                },
            ))
            .expect("resume");
        assert_eq!(state.status, RunStatus::Running);
    }

    #[test]
    fn plan_reconciliation_activates_revision_and_discovers_ready_work() {
        let mut state = run();
        state
            .handle(command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ))
            .expect("start");

        let events = state
            .handle(command(
                "reconcile",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-2".to_owned(),
                    discovered: vec![id("task-b")],
                    ready: vec![id("task-b")],
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ))
            .expect("reconcile");

        assert_eq!(events.len(), 4);
        assert_eq!(state.plan_revision, "plan-2");
        assert_eq!(
            state.task(&id("task-b")).expect("task").status,
            TaskStatus::Scheduled
        );
    }

    #[test]
    fn repeated_reconciliation_is_a_no_op() {
        let mut state = run();
        state
            .handle(command(
                "start",
                CommandKind::StartRun {
                    now: "2026-09-29T00:00:00Z".to_owned(),
                },
            ))
            .expect("start");
        state
            .handle(command(
                "reconcile",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-2".to_owned(),
                    discovered: vec![id("task-b")],
                    ready: vec![id("task-b")],
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:01Z".to_owned(),
                },
            ))
            .expect("first reconcile");

        let events = state
            .handle(command(
                "reconcile-again",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-2".to_owned(),
                    discovered: Vec::new(),
                    ready: Vec::new(),
                    invalidated: Vec::new(),
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:02Z".to_owned(),
                },
            ))
            .expect("second reconcile");
        assert!(events.is_empty());
    }

    #[test]
    fn plan_removal_is_explicit_and_does_not_erase_history() {
        let mut state = run();
        start_and_schedule(&mut state);
        let events = state
            .handle(command(
                "invalidate",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-2".to_owned(),
                    discovered: Vec::new(),
                    ready: Vec::new(),
                    invalidated: vec![id("task-a")],
                    superseded: Vec::new(),
                    now: "2026-09-29T00:00:05Z".to_owned(),
                },
            ))
            .expect("invalidate");
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, EventKind::TaskInvalidated { .. }))
        );
        assert_eq!(
            state.task(&id("task-a")).expect("task").status,
            TaskStatus::Invalidated
        );
        assert_eq!(state.history_version, 6);
    }

    #[test]
    fn supersession_uses_a_new_task_identity() {
        let mut state = run();
        start_and_schedule(&mut state);
        let events = state
            .handle(command(
                "supersede",
                CommandKind::ReconcilePlan {
                    new_plan_revision: "plan-2".to_owned(),
                    discovered: vec![id("task-b")],
                    ready: vec![id("task-b")],
                    invalidated: Vec::new(),
                    superseded: vec![TaskSupersession {
                        task_id: id("task-a"),
                        replacement_task_id: id("task-b"),
                        reason: "tool contract changed".to_owned(),
                    }],
                    now: "2026-09-29T00:00:05Z".to_owned(),
                },
            ))
            .expect("supersede");

        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, EventKind::TaskSuperseded { .. }))
        );
        assert_eq!(
            state.task(&id("task-a")).expect("old task").status,
            TaskStatus::Invalidated
        );
        assert_eq!(
            state.task(&id("task-b")).expect("replacement").status,
            TaskStatus::Scheduled
        );
        assert_ne!(
            state.task(&id("task-a")).expect("old task").task_id,
            state.task(&id("task-b")).expect("replacement").task_id
        );
    }

    #[test]
    fn timeout_creates_a_timer_and_timer_fire_reschedules_the_task() {
        let mut state = run();
        start_and_schedule(&mut state);
        state
            .handle(command(
                "claim",
                CommandKind::ClaimTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:03Z".to_owned(),
                },
            ))
            .expect("claim");
        state
            .handle(command(
                "start-attempt",
                CommandKind::StartAttempt {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    now: "2026-09-29T00:00:04Z".to_owned(),
                },
            ))
            .expect("start attempt");
        let timeout_events = state
            .handle(command(
                "timeout",
                CommandKind::TimeoutTask {
                    task_id: id("task-a"),
                    attempt_id: attempt("attempt-1"),
                    worker: actor("worker-a"),
                    timer_id: super::TimerId::new("timer-1").expect("timer"),
                    retry_at: "2026-09-29T00:00:10Z".to_owned(),
                    now: "2026-09-29T00:00:05Z".to_owned(),
                },
            ))
            .expect("timeout");
        assert_eq!(timeout_events.len(), 3);
        assert_eq!(
            state.task(&id("task-a")).expect("task").status,
            TaskStatus::Waiting
        );
        assert!(
            !state
                .timers
                .get(&super::TimerId::new("timer-1").expect("timer"))
                .expect("timer state")
                .fired
        );

        state
            .handle(command(
                "fire-timer",
                CommandKind::FireTimer {
                    timer_id: super::TimerId::new("timer-1").expect("timer"),
                    now: "2026-09-29T00:00:11Z".to_owned(),
                },
            ))
            .expect("fire timer");
        assert_eq!(
            state.task(&id("task-a")).expect("task").status,
            TaskStatus::Scheduled
        );
        assert!(
            state
                .timers
                .get(&super::TimerId::new("timer-1").expect("timer"))
                .expect("timer state")
                .fired
        );
    }
}
