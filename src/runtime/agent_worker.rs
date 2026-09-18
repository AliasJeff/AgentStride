use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    runtime::token::RuntimeTaskToken,
    runtime::{Event, RunId, TaskId},
    store::{RuntimeTaskLease, Store, StoreError},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Transient,
    Permanent,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentOutcome {
    Completed {
        result: Value,
    },
    Failed {
        kind: ErrorKind,
        message: String,
    },
    Checkpointed {
        progress: Value,
    },
    ProposedPlan {
        proposal_id: String,
    },
    WaitingForApproval,
    HandedOff {
        to_worker: Option<crate::domain::Actor>,
        summary: String,
        artifacts: String,
        next_step: String,
    },
    Crashed {
        step: u32,
    },
}

pub trait AgentWorker {
    fn execute(
        &mut self,
        lease: &RuntimeTaskLease,
        session: &mut AgentSession<'_>,
    ) -> Result<AgentOutcome, AgentWorkerError>;
}

#[derive(Debug, Error)]
pub enum AgentWorkerError {
    #[error("agent worker failed: {message}")]
    Provider { message: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Token(#[from] crate::runtime::token::RuntimeTokenError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentContext {
    pub run_id: RunId,
    pub plan_revision: String,
    pub task_id: TaskId,
    pub attempt: u32,
    pub checkpoint: Option<Value>,
    pub recent_events: Vec<Event>,
}

pub struct AgentSession<'a> {
    store: &'a mut Store,
    token: RuntimeTaskToken,
}

impl<'a> AgentSession<'a> {
    fn new(store: &'a mut Store, token: RuntimeTaskToken) -> Self {
        Self { store, token }
    }

    pub fn heartbeat(
        &mut self,
        now: &str,
        lease_until: &str,
    ) -> Result<RuntimeTaskLease, AgentWorkerError> {
        let lease = self
            .store
            .heartbeat_runtime_task(&self.token, now, lease_until)?;
        self.token = self.store.issue_runtime_task_token(&lease)?;
        Ok(lease)
    }

    pub fn checkpoint(&self) -> Result<Option<Value>, AgentWorkerError> {
        let state = self.store.runtime_state(&self.token.run_id)?;
        Ok(state
            .task(&self.token.task_id)
            .map_err(StoreError::from)?
            .checkpoint
            .clone())
    }

    pub fn context(&self, lease: &RuntimeTaskLease) -> Result<AgentContext, AgentWorkerError> {
        let state = self.store.runtime_state(&self.token.run_id)?;
        let since = state.history_version.saturating_sub(20);
        let recent_events = self
            .store
            .runtime_events(&self.token.run_id, since, 20)?
            .events;
        Ok(AgentContext {
            run_id: lease.run_id.clone(),
            plan_revision: state.plan_revision.clone(),
            task_id: lease.task_id.clone(),
            attempt: lease.attempt,
            checkpoint: state
                .task(&lease.task_id)
                .map_err(StoreError::from)?
                .checkpoint
                .clone(),
            recent_events,
        })
    }

    fn checkpoint_task(&mut self, progress: Value, now: &str) -> Result<(), AgentWorkerError> {
        self.store
            .checkpoint_runtime_task(&self.token, progress, now)?;
        Ok(())
    }

    fn into_token(self) -> RuntimeTaskToken {
        self.token
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRunResult {
    Idle,
    Completed {
        durable_task_id: String,
        task_id: String,
    },
    Failed {
        durable_task_id: String,
        task_id: String,
        error: String,
    },
    RetryScheduled {
        durable_task_id: String,
        task_id: String,
        retry_at: String,
    },
    Checkpointed {
        durable_task_id: String,
        task_id: String,
        progress: Value,
    },
    Waiting {
        durable_task_id: String,
        task_id: String,
        reason: String,
    },
    HandedOff {
        durable_task_id: String,
        task_id: String,
        to_worker: Option<String>,
    },
    Crashed {
        durable_task_id: String,
        task_id: String,
        step: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_seconds: i64,
    pub max_backoff_seconds: i64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_seconds: 5,
            max_backoff_seconds: 300,
        }
    }
}

impl RetryPolicy {
    fn delay_seconds(&self, attempt: u32) -> Result<i64, AgentRunnerError> {
        if self.max_attempts == 0
            || self.initial_backoff_seconds < 0
            || self.max_backoff_seconds < self.initial_backoff_seconds
        {
            return Err(AgentRunnerError::InvalidRetryPolicy);
        }
        let exponent = attempt.saturating_sub(1).min(62);
        let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
        Ok(self
            .initial_backoff_seconds
            .saturating_mul(multiplier)
            .min(self.max_backoff_seconds))
    }
}

pub struct AgentRunner<W> {
    worker: W,
    actor: crate::domain::Actor,
    retry_policy: RetryPolicy,
}

impl<W> AgentRunner<W>
where
    W: AgentWorker,
{
    pub fn new(worker: W, actor: crate::domain::Actor) -> Self {
        Self {
            worker,
            actor,
            retry_policy: RetryPolicy::default(),
        }
    }

    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn run_once(
        &mut self,
        store: &mut Store,
        now: &str,
        lease_until: &str,
    ) -> Result<AgentRunResult, AgentRunnerError> {
        self.run_until_terminal(store, now, lease_until, 1)
    }

    pub fn run_until_terminal(
        &mut self,
        store: &mut Store,
        now: &str,
        lease_until: &str,
        max_steps: u32,
    ) -> Result<AgentRunResult, AgentRunnerError> {
        if max_steps == 0 {
            return Err(AgentRunnerError::InvalidStepLimit);
        }
        let (lease, _) = match store.poll_runtime_task(self.actor.clone(), now, lease_until) {
            Ok(value) => value,
            Err(StoreError::NoRuntimeTask) => return Ok(AgentRunResult::Idle),
            Err(error) => return Err(error.into()),
        };
        let token = store.issue_runtime_task_token(&lease)?;
        store.start_runtime_task(&token, now)?;
        let mut session = AgentSession::new(store, token);
        let durable_task_id = lease.durable_task_id.clone();
        let task_id = lease.task_id.to_string();
        for step in 0..max_steps {
            let outcome = self.worker.execute(&lease, &mut session)?;
            match outcome {
                AgentOutcome::Checkpointed { progress } => {
                    session.checkpoint_task(progress.clone(), now)?;
                    if step + 1 == max_steps {
                        return Ok(AgentRunResult::Checkpointed {
                            durable_task_id,
                            task_id,
                            progress,
                        });
                    }
                }
                AgentOutcome::Completed { result } => {
                    let token = session.into_token();
                    store.complete_runtime_task(&token, result, now)?;
                    return Ok(AgentRunResult::Completed {
                        durable_task_id,
                        task_id,
                    });
                }
                AgentOutcome::Failed { kind, message } => {
                    let token = session.into_token();
                    if kind == ErrorKind::Transient
                        && lease.attempt < self.retry_policy.max_attempts
                    {
                        let delay = self.retry_policy.delay_seconds(lease.attempt)?;
                        let retry_at = retry_at(now, delay)?;
                        let timer_id = crate::runtime::TimerId::new(format!(
                            "retry-{}-{}",
                            lease.durable_task_id, lease.attempt
                        ))
                        .map_err(AgentRunnerError::from)?;
                        store.retry_runtime_task(&token, timer_id, &retry_at, &message, now)?;
                        return Ok(AgentRunResult::RetryScheduled {
                            durable_task_id,
                            task_id,
                            retry_at,
                        });
                    }
                    store.fail_runtime_task(&token, &message, now)?;
                    return Ok(AgentRunResult::Failed {
                        durable_task_id,
                        task_id,
                        error: message,
                    });
                }
                AgentOutcome::ProposedPlan { proposal_id } => {
                    let token = session.into_token();
                    let reason = format!("waiting for approval of plan proposal {proposal_id}");
                    store.wait_runtime_task(&token, &reason, now)?;
                    return Ok(AgentRunResult::Waiting {
                        durable_task_id,
                        task_id,
                        reason,
                    });
                }
                AgentOutcome::WaitingForApproval => {
                    let token = session.into_token();
                    let reason = "waiting for plan approval".to_owned();
                    store.wait_runtime_task(&token, &reason, now)?;
                    return Ok(AgentRunResult::Waiting {
                        durable_task_id,
                        task_id,
                        reason,
                    });
                }
                AgentOutcome::HandedOff {
                    to_worker,
                    summary,
                    artifacts,
                    next_step,
                } => {
                    let handoff_target = to_worker.as_ref().map(|actor| actor.as_str().to_owned());
                    let token = session.into_token();
                    store.handoff_runtime_task(
                        &token, to_worker, &summary, &artifacts, &next_step, now,
                    )?;
                    return Ok(AgentRunResult::HandedOff {
                        durable_task_id,
                        task_id,
                        to_worker: handoff_target,
                    });
                }
                AgentOutcome::Crashed { step } => {
                    return Ok(AgentRunResult::Crashed {
                        durable_task_id,
                        task_id,
                        step,
                    });
                }
            }
        }
        unreachable!("run loop returns on terminal outcome or final checkpoint")
    }
}

#[derive(Debug, Error)]
pub enum AgentRunnerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Worker(#[from] AgentWorkerError),
    #[error("invalid retry policy")]
    InvalidRetryPolicy,
    #[error("agent step limit must be greater than zero")]
    InvalidStepLimit,
    #[error("invalid runtime timestamp `{value}`")]
    InvalidTimestamp { value: String },
    #[error(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),
}

fn retry_at(now: &str, delay_seconds: i64) -> Result<String, AgentRunnerError> {
    let timestamp =
        OffsetDateTime::parse(now, &Rfc3339).map_err(|_| AgentRunnerError::InvalidTimestamp {
            value: now.to_owned(),
        })?;
    (timestamp + Duration::seconds(delay_seconds))
        .format(&Rfc3339)
        .map_err(|_| AgentRunnerError::InvalidTimestamp {
            value: now.to_owned(),
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeWorkerConfig {
    pub seed: u64,
    pub failures_before_success: u32,
    pub crash_at_step: Option<u32>,
    pub duration_steps: u32,
    pub heartbeat_every: Option<u32>,
    pub proposal_id: Option<String>,
    pub handoff_to: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FakeWorker {
    config: FakeWorkerConfig,
}

impl FakeWorker {
    pub fn new(config: FakeWorkerConfig) -> Self {
        Self { config }
    }

    pub fn outcome(&self, task_id: &str, attempt: u32, completed_steps: u32) -> AgentOutcome {
        if let Some(crash_at_step) = self.config.crash_at_step
            && completed_steps >= crash_at_step
        {
            return AgentOutcome::Crashed {
                step: crash_at_step,
            };
        }
        if attempt <= self.config.failures_before_success {
            return AgentOutcome::Failed {
                kind: ErrorKind::Transient,
                message: format!(
                    "deterministic failure for task {task_id}, attempt {attempt}, seed {}",
                    self.config.seed
                ),
            };
        }
        if completed_steps < self.config.duration_steps {
            return AgentOutcome::Checkpointed {
                progress: json!({
                    "task_id": task_id,
                    "step": completed_steps + 1,
                    "total_steps": self.config.duration_steps,
                    "seed": self.config.seed,
                }),
            };
        }
        if let Some(proposal_id) = &self.config.proposal_id {
            return AgentOutcome::ProposedPlan {
                proposal_id: proposal_id.clone(),
            };
        }
        if let Some(to_worker) = &self.config.handoff_to {
            return AgentOutcome::HandedOff {
                to_worker: Some(
                    crate::domain::Actor::new(to_worker.clone())
                        .expect("FakeWorker handoff target must be a valid actor"),
                ),
                summary: format!("durable handoff from task {task_id}"),
                artifacts: String::new(),
                next_step: "continue from checkpoint and execution history".to_owned(),
            };
        }
        AgentOutcome::Completed {
            result: json!({
                "task_id": task_id,
                "attempt": attempt,
                "seed": self.config.seed,
            }),
        }
    }

    pub fn should_heartbeat(&self, step: u32) -> bool {
        self.config
            .heartbeat_every
            .is_some_and(|interval| interval > 0 && step > 0 && step.is_multiple_of(interval))
    }
}

impl AgentWorker for FakeWorker {
    fn execute(
        &mut self,
        lease: &RuntimeTaskLease,
        session: &mut AgentSession<'_>,
    ) -> Result<AgentOutcome, AgentWorkerError> {
        let completed_steps = session
            .checkpoint()?
            .and_then(|value| value.get("step").and_then(Value::as_u64))
            .unwrap_or(0) as u32;
        Ok(self.outcome(lease.task_id.as_str(), lease.attempt, completed_steps))
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentOutcome, FakeWorker, FakeWorkerConfig};

    #[test]
    fn same_seed_produces_same_outcome_history() {
        let config = FakeWorkerConfig {
            seed: 42,
            failures_before_success: 1,
            crash_at_step: None,
            duration_steps: 0,
            heartbeat_every: Some(2),
            proposal_id: None,
            handoff_to: None,
        };
        let first = FakeWorker::new(config.clone());
        let second = FakeWorker::new(config);
        assert_eq!(
            first.outcome("task-a", 1, 0),
            second.outcome("task-a", 1, 0)
        );
        assert_eq!(
            first.outcome("task-a", 2, 0),
            second.outcome("task-a", 2, 0)
        );
    }

    #[test]
    fn configured_crash_precedes_completion() {
        let worker = FakeWorker::new(FakeWorkerConfig {
            seed: 7,
            failures_before_success: 0,
            crash_at_step: Some(3),
            duration_steps: 0,
            heartbeat_every: None,
            proposal_id: None,
            handoff_to: None,
        });
        assert_eq!(
            worker.outcome("task-a", 1, 3),
            AgentOutcome::Crashed { step: 3 }
        );
    }

    #[test]
    fn duration_heartbeat_and_proposal_are_deterministic_outcomes() {
        let worker = FakeWorker::new(FakeWorkerConfig {
            seed: 9,
            failures_before_success: 0,
            crash_at_step: None,
            duration_steps: 2,
            heartbeat_every: Some(2),
            proposal_id: Some("proposal-1".to_owned()),
            handoff_to: None,
        });
        assert!(matches!(
            worker.outcome("task-a", 1, 0),
            AgentOutcome::Checkpointed { .. }
        ));
        assert!(worker.should_heartbeat(2));
        assert!(matches!(
            worker.outcome("task-a", 1, 2),
            AgentOutcome::ProposedPlan { .. }
        ));
    }
}
