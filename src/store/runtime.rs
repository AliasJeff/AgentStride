use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::Actor,
    runtime::{
        AttemptId, Command, CommandKind, Event, EventKind, MutableState, RunId, TaskId, decide,
        token::RuntimeTaskToken,
    },
};

use super::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStateResult {
    pub state: MutableState,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEventPage {
    pub events: Vec<Event>,
    pub head: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRunSummary {
    pub run_id: RunId,
    pub plan_revision: String,
    pub status: crate::runtime::RunStatus,
    pub history_version: u64,
    pub task_count: usize,
    pub succeeded_tasks: usize,
    pub active_tasks: usize,
    pub waiting_tasks: usize,
    pub failed_tasks: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeTaskLease {
    pub durable_task_id: String,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub attempt: u32,
    pub task_kind: String,
    pub worker: Actor,
    pub history_version: u64,
    pub lease_until: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeVerification {
    pub run_id: RunId,
    pub valid: bool,
    pub history_version: u64,
    pub event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecoveryReport {
    pub run_id: RunId,
    pub inspected: usize,
    pub recovered: Vec<String>,
    pub state: MutableState,
}

struct EventMetadata {
    event_type: &'static str,
    task_id: Option<String>,
    attempt_id: Option<String>,
    actor: Option<String>,
    payload: Value,
}

type EventRow = (
    u64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

impl Store {
    pub fn start_run(
        &mut self,
        run_id: RunId,
        plan_revision: &str,
        command: Command,
    ) -> Result<RuntimeStateResult, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: Option<String> = transaction
            .query_row(
                "SELECT run_id FROM runtime_runs WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(StoreError::RuntimeRunExists {
                run_id: run_id.to_string(),
            });
        }

        let mut state = MutableState::new(run_id, plan_revision.to_owned())?;
        let events = decide(&state, command)?;
        apply_events(&mut state, &events)?;
        insert_run(&transaction, &state, &events)?;
        transaction.commit()?;
        Ok(RuntimeStateResult { state, events })
    }

    pub fn apply_runtime_command(
        &mut self,
        run_id: &RunId,
        command: Command,
    ) -> Result<RuntimeStateResult, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = apply_runtime_command_in_transaction(&transaction, run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn issue_runtime_task_token(
        &self,
        lease: &RuntimeTaskLease,
    ) -> Result<RuntimeTaskToken, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        Ok(RuntimeTaskToken::new(
            workspace_id,
            lease.run_id.clone(),
            lease.durable_task_id.clone(),
            lease.task_id.clone(),
            lease.attempt_id.clone(),
            lease.worker.clone(),
            lease.history_version,
            lease.lease_until.clone(),
        )?)
    }

    pub fn complete_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        result: Value,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT run_id, task_id, attempt_id, lease_owner, lease_until, status
                 FROM runtime_tasks
                 WHERE durable_task_id = ?1",
                params![&token.durable_task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            })?;
        validate_runtime_task_token(token, now, &current)?;

        let command = Command::new(
            format!("complete-{}", token.attempt_id),
            CommandKind::CompleteTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                result,
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn checkpoint_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        progress: Value,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_runtime_lease(&transaction, token, now)?;
        let history_version: u64 = transaction.query_row(
            "SELECT history_version FROM runtime_runs WHERE run_id = ?1",
            params![token.run_id.as_str()],
            |row| row.get(0),
        )?;
        let command = Command::new(
            format!("checkpoint-{}-{}", token.attempt_id, history_version + 1),
            CommandKind::CheckpointTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                progress,
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn wait_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        reason: &str,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if reason.trim().is_empty() || now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyFailure.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_runtime_lease(&transaction, token, now)?;
        let command = Command::new(
            format!("wait-{}", token.attempt_id),
            CommandKind::WaitTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                reason: reason.to_owned(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn handoff_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        to_worker: Option<Actor>,
        summary: &str,
        artifacts: &str,
        next_step: &str,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if summary.trim().is_empty() || next_step.trim().is_empty() || now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyFailure.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_runtime_lease(&transaction, token, now)?;
        let command = Command::new(
            format!("handoff-{}", token.attempt_id),
            CommandKind::HandoffTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                to_worker,
                summary: summary.to_owned(),
                artifacts: artifacts.to_owned(),
                next_step: next_step.to_owned(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn retry_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        timer_id: crate::runtime::TimerId,
        retry_at: &str,
        error: &str,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if retry_at.trim().is_empty() || error.trim().is_empty() || now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyFailure.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_runtime_lease(&transaction, token, now)?;
        let command = Command::new(
            format!("retry-{}", token.attempt_id),
            CommandKind::RetryTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                timer_id,
                retry_at: retry_at.to_owned(),
                error: error.to_owned(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn fail_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        error: &str,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if error.trim().is_empty() || now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyFailure.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT run_id, task_id, attempt_id, lease_owner, lease_until, status
                 FROM runtime_tasks
                 WHERE durable_task_id = ?1",
                params![&token.durable_task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            })?;
        validate_runtime_task_token(token, now, &current)?;

        let command = Command::new(
            format!("fail-{}", token.attempt_id),
            CommandKind::FailTask {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                error: error.to_owned(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn start_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT run_id, task_id, attempt_id, lease_owner, lease_until, status
                 FROM runtime_tasks
                 WHERE durable_task_id = ?1",
                params![&token.durable_task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            })?;
        validate_runtime_task_token(token, now, &current)?;

        let command = Command::new(
            format!("start-{}", token.attempt_id),
            CommandKind::StartAttempt {
                task_id: token.task_id.clone(),
                attempt_id: token.attempt_id.clone(),
                worker: token.worker.clone(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &token.run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn heartbeat_runtime_task(
        &mut self,
        token: &RuntimeTaskToken,
        now: &str,
        lease_until: &str,
    ) -> Result<RuntimeTaskLease, StoreError> {
        let workspace_id = self.ensure_workspace_id()?;
        token.verify_signature(&workspace_id)?;
        if now.trim().is_empty() || lease_until.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT task_kind, payload
                 FROM runtime_tasks
                 WHERE durable_task_id = ?1 AND run_id = ?2
                   AND task_id = ?3 AND attempt_id = ?4
                   AND lease_owner = ?5 AND lease_until = ?6
                   AND status = 'leased'",
                params![
                    &token.durable_task_id,
                    token.run_id.as_str(),
                    token.task_id.as_str(),
                    token.attempt_id.as_str(),
                    token.worker.as_str(),
                    &token.lease_until,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            })?;
        if token.lease_until.as_str() <= now {
            return Err(StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            });
        }
        let changed = transaction.execute(
            "UPDATE runtime_tasks
             SET lease_until = ?1, updated_at = ?2
             WHERE durable_task_id = ?3 AND lease_owner = ?4
               AND attempt_id = ?5 AND lease_until = ?6
               AND status = 'leased'",
            params![
                lease_until,
                now,
                &token.durable_task_id,
                token.worker.as_str(),
                token.attempt_id.as_str(),
                &token.lease_until,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RuntimeTaskConflict {
                task_id: token.task_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(RuntimeTaskLease {
            durable_task_id: token.durable_task_id.clone(),
            run_id: token.run_id.clone(),
            task_id: token.task_id.clone(),
            attempt_id: token.attempt_id.clone(),
            attempt: token
                .attempt_id
                .as_str()
                .rsplit_once('-')
                .and_then(|(_, value)| value.parse().ok())
                .unwrap_or_default(),
            task_kind: current.0,
            worker: token.worker.clone(),
            history_version: token.history_version,
            lease_until: lease_until.to_owned(),
            payload: serde_json::from_str(&current.1)?,
        })
    }

    pub fn timeout_runtime_task(
        &mut self,
        durable_task_id: &str,
        timer_id: crate::runtime::TimerId,
        retry_at: &str,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        if durable_task_id.trim().is_empty() || retry_at.trim().is_empty() || now.trim().is_empty()
        {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT run_id, task_id, attempt_id, lease_owner, lease_until
                 FROM runtime_tasks
                 WHERE durable_task_id = ?1 AND status = 'leased'",
                params![durable_task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTaskConflict {
                task_id: durable_task_id.to_owned(),
            })?;
        if current.4.as_str() > now {
            return Err(StoreError::RuntimeTaskConflict { task_id: current.1 });
        }
        let run_id = RunId::new(current.0)?;
        let task_id = TaskId::new(current.1)?;
        let attempt_id = AttemptId::new(current.2)?;
        let worker = Actor::new(current.3)?;
        let command = Command::new(
            format!("timeout-{durable_task_id}"),
            CommandKind::TimeoutTask {
                task_id,
                attempt_id,
                worker,
                timer_id,
                retry_at: retry_at.to_owned(),
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn fire_runtime_timer(
        &mut self,
        timer_id: crate::runtime::TimerId,
        now: &str,
    ) -> Result<RuntimeStateResult, StoreError> {
        if now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run_id: String = transaction
            .query_row(
                "SELECT run_id FROM runtime_timers
                 WHERE timer_id = ?1 AND status = 'pending'",
                params![timer_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeTimerConflict {
                timer_id: timer_id.to_string(),
            })?;
        let run_id = RunId::new(run_id)?;
        let command = Command::new(
            format!("fire-{timer_id}"),
            CommandKind::FireTimer {
                timer_id,
                now: now.to_owned(),
            },
        )?;
        let result = apply_runtime_command_in_transaction(&transaction, &run_id, command)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn rebuild_runtime_state(&self, run_id: &RunId) -> Result<MutableState, StoreError> {
        let events = self.runtime_events(run_id, 0, usize::MAX)?.events;
        let plan_revision = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::RunStarted { plan_revision } => Some(plan_revision.clone()),
                _ => None,
            })
            .ok_or_else(|| StoreError::RuntimeRunNotFound {
                run_id: run_id.to_string(),
            })?;
        let mut rebuilt = MutableState::new(run_id.clone(), plan_revision)?;
        for event in &events {
            crate::runtime::apply(&mut rebuilt, event)?;
        }
        Ok(rebuilt)
    }

    pub fn verify_runtime_state(&self, run_id: &RunId) -> Result<RuntimeVerification, StoreError> {
        let rebuilt = self.rebuild_runtime_state(run_id)?;
        let materialized = match self.runtime_state(run_id) {
            Ok(state) => Some(state),
            Err(StoreError::Json(_)) => None,
            Err(error) => return Err(error),
        };
        let event_count = rebuilt.history_version as usize;
        Ok(RuntimeVerification {
            run_id: run_id.clone(),
            valid: materialized.as_ref() == Some(&rebuilt),
            history_version: rebuilt.history_version,
            event_count,
        })
    }

    pub fn recover_expired_runtime_tasks(
        &mut self,
        run_id: &RunId,
        now: &str,
        retry_delay_seconds: i64,
    ) -> Result<RuntimeRecoveryReport, StoreError> {
        if now.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }
        if retry_delay_seconds < 0 {
            return Err(crate::runtime::RuntimeError::InvalidRetryDelay {
                seconds: retry_delay_seconds,
            }
            .into());
        }
        let retry_at = retry_at(now, retry_delay_seconds)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = {
            let mut statement = transaction.prepare(
                "SELECT durable_task_id
                 FROM runtime_tasks
                 WHERE run_id = ?1 AND status = 'leased' AND lease_until <= ?2
                 ORDER BY durable_task_id",
            )?;
            let rows = statement
                .query_map(params![run_id.as_str(), now], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut recovered = Vec::new();
        for durable_task_id in &expired {
            let current = transaction
                .query_row(
                    "SELECT task_id, attempt_id, lease_owner
                     FROM runtime_tasks
                     WHERE durable_task_id = ?1 AND status = 'leased'",
                    params![durable_task_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((raw_task_id, raw_attempt_id, raw_worker)) = current else {
                continue;
            };
            let command = Command::new(
                format!("recover-{durable_task_id}"),
                CommandKind::TimeoutTask {
                    task_id: TaskId::new(raw_task_id)?,
                    attempt_id: AttemptId::new(raw_attempt_id)?,
                    worker: Actor::new(raw_worker)?,
                    timer_id: crate::runtime::TimerId::new(format!("timer-{durable_task_id}"))?,
                    retry_at: retry_at.clone(),
                    now: now.to_owned(),
                },
            )?;
            apply_runtime_command_in_transaction(&transaction, run_id, command)?;
            recovered.push(durable_task_id.clone());
        }
        let state = load_state(&transaction, run_id)?;
        transaction.commit()?;
        Ok(RuntimeRecoveryReport {
            run_id: run_id.clone(),
            inspected: expired.len(),
            recovered,
            state,
        })
    }

    pub fn runtime_state(&self, run_id: &RunId) -> Result<MutableState, StoreError> {
        let state_json: String = self
            .connection
            .query_row(
                "SELECT state_json FROM runtime_runs WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeRunNotFound {
                run_id: run_id.to_string(),
            })?;
        Ok(serde_json::from_str(&state_json)?)
    }

    pub fn list_runtime_runs(&self) -> Result<Vec<RuntimeRunSummary>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT state_json FROM runtime_runs ORDER BY run_id")?;
        let states = statement
            .query_map([], |row| {
                let state_json: String = row.get(0)?;
                Ok(state_json)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        states
            .into_iter()
            .map(|state_json| {
                let state: MutableState = serde_json::from_str(&state_json)?;
                let mut summary = RuntimeRunSummary {
                    run_id: state.run_id,
                    plan_revision: state.plan_revision,
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
                        crate::runtime::TaskStatus::Succeeded => summary.succeeded_tasks += 1,
                        crate::runtime::TaskStatus::Running
                        | crate::runtime::TaskStatus::Claimed
                        | crate::runtime::TaskStatus::Scheduled => summary.active_tasks += 1,
                        crate::runtime::TaskStatus::Waiting => summary.waiting_tasks += 1,
                        crate::runtime::TaskStatus::Failed => summary.failed_tasks += 1,
                        _ => {}
                    }
                }
                Ok(summary)
            })
            .collect()
    }

    pub fn runtime_events(
        &self,
        run_id: &RunId,
        since: u64,
        limit: usize,
    ) -> Result<RuntimeEventPage, StoreError> {
        let head: u64 = self
            .connection
            .query_row(
                "SELECT history_version FROM runtime_runs WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RuntimeRunNotFound {
                run_id: run_id.to_string(),
            })?;

        let mut statement = self.connection.prepare(
            "SELECT event_seq, event_id, event_type, plan_revision,
                    task_id, attempt_id, actor, occurred_at, payload
             FROM execution_events
             WHERE run_id = ?1 AND event_seq > ?2
             ORDER BY event_seq
             LIMIT ?3",
        )?;
        let safe_limit = limit.min(i64::MAX as usize) as i64;
        let rows = statement.query_map(params![run_id.as_str(), since, safe_limit], |row| {
            let seq: u64 = row.get(0)?;
            let event_id: String = row.get(1)?;
            let event_type: String = row.get(2)?;
            let plan_revision: String = row.get(3)?;
            let task_id: Option<String> = row.get(4)?;
            let attempt_id: Option<String> = row.get(5)?;
            let actor: Option<String> = row.get(6)?;
            let occurred_at: String = row.get(7)?;
            let payload: String = row.get(8)?;
            Ok((
                seq,
                event_id,
                event_type,
                plan_revision,
                task_id,
                attempt_id,
                actor,
                occurred_at,
                payload,
            ))
        })?;

        let rows = rows
            .map(|row| {
                row.map_err(StoreError::from)
                    .and_then(|row| decode_event(run_id, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RuntimeEventPage { events: rows, head })
    }
    pub fn poll_runtime_task(
        &mut self,
        worker: Actor,
        now: &str,
        lease_until: &str,
    ) -> Result<(RuntimeTaskLease, RuntimeStateResult), StoreError> {
        if now.trim().is_empty() || lease_until.trim().is_empty() {
            return Err(crate::runtime::RuntimeError::EmptyTimestamp.into());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = transaction
            .query_row(
                "SELECT durable_task_id, run_id, task_id, task_kind,
                        attempts, payload
                 FROM runtime_tasks
                 WHERE status = 'ready' AND available_at <= ?1
                   AND (preferred_worker IS NULL OR preferred_worker = ?2)
                 ORDER BY available_at, durable_task_id
                 LIMIT 1",
                params![now, worker.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NoRuntimeTask)?;
        let (durable_task_id, raw_run_id, raw_task_id, task_kind, attempts, payload) = task;
        let run_id = RunId::new(raw_run_id)?;
        let task_id = TaskId::new(raw_task_id)?;
        let attempt_id = AttemptId::new(format!(
            "attempt-{durable_task_id}-{}",
            attempts.saturating_add(1)
        ))?;
        let mut state = load_state(&transaction, &run_id)?;
        let command = Command::new(
            format!("claim-{durable_task_id}-{}", attempts.saturating_add(1)),
            CommandKind::ClaimTask {
                task_id: task_id.clone(),
                attempt_id: attempt_id.clone(),
                worker: worker.clone(),
                now: now.to_owned(),
            },
        )?;
        let events = decide(&state, command)?;
        apply_events(&mut state, &events)?;
        append_events(&transaction, &state, &events)?;
        let changed = transaction.execute(
            "UPDATE runtime_tasks
             SET status = 'leased', attempt_id = ?1, lease_owner = ?2,
                 lease_until = ?3, preferred_worker = NULL,
                 attempts = attempts + 1, updated_at = ?4
             WHERE durable_task_id = ?5 AND status = 'ready'
                   AND available_at <= ?6",
            params![
                attempt_id.as_str(),
                worker.as_str(),
                lease_until,
                now,
                &durable_task_id,
                now,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RuntimeTaskConflict {
                task_id: task_id.to_string(),
            });
        }
        update_state(&transaction, &state, &events)?;
        transaction.commit()?;
        let logical_attempt = state
            .tasks
            .get(&task_id)
            .map(|task| task.attempt)
            .unwrap_or_else(|| attempts.saturating_add(1));
        let lease = RuntimeTaskLease {
            durable_task_id,
            run_id,
            task_id,
            attempt_id,
            attempt: logical_attempt,
            task_kind,
            worker,
            history_version: state.history_version,
            lease_until: lease_until.to_owned(),
            payload: serde_json::from_str(&payload)?,
        };
        Ok((lease, RuntimeStateResult { state, events }))
    }
}

fn apply_runtime_command_in_transaction(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    command: Command,
) -> Result<RuntimeStateResult, StoreError> {
    let mut state = load_state(transaction, run_id)?;
    if let CommandKind::ReconcilePlan {
        new_plan_revision, ..
    } = &command.kind
        && new_plan_revision != &state.plan_revision
    {
        super::plan::ensure_approved_plan_revision_in_transaction(
            transaction,
            run_id.as_str(),
            &state.plan_revision,
            new_plan_revision,
        )?;
    }
    let events = decide(&state, command)?;
    if events.is_empty() {
        return Ok(RuntimeStateResult { state, events });
    }
    apply_events(&mut state, &events)?;
    append_events(transaction, &state, &events)?;
    apply_task_queue_changes(transaction, &state, &events)?;
    update_state(transaction, &state, &events)?;
    Ok(RuntimeStateResult { state, events })
}

fn ensure_active_runtime_lease(
    transaction: &Transaction<'_>,
    token: &RuntimeTaskToken,
    now: &str,
) -> Result<(), StoreError> {
    let current = transaction
        .query_row(
            "SELECT run_id, task_id, attempt_id, lease_owner, lease_until, status
             FROM runtime_tasks
             WHERE durable_task_id = ?1",
            params![&token.durable_task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::RuntimeTaskConflict {
            task_id: token.task_id.to_string(),
        })?;
    validate_runtime_task_token(token, now, &current)
}

fn validate_runtime_task_token(
    token: &RuntimeTaskToken,
    now: &str,
    current: &(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ),
) -> Result<(), StoreError> {
    if current.5 != "leased" {
        return Err(StoreError::RuntimeTaskConflict {
            task_id: token.task_id.to_string(),
        });
    }
    let effective_lease_until = current.4.as_deref().unwrap_or(token.lease_until.as_str());
    if token.lease_until.as_str() <= now || effective_lease_until <= now {
        return Err(crate::runtime::RuntimeError::LeaseExpired {
            task_id: token.task_id.to_string(),
            lease_until: token.lease_until.to_string(),
            now: now.to_owned(),
        }
        .into());
    }
    if current.2.as_deref() != Some(token.attempt_id.as_str()) {
        return Err(crate::runtime::RuntimeError::AttemptMismatch {
            expected: current.2.clone(),
            actual: token.attempt_id.to_string(),
        }
        .into());
    }
    if current.0 != token.run_id.as_str()
        || current.1 != token.task_id.as_str()
        || current.3.as_deref() != Some(token.worker.as_str())
        || current.4.as_deref() != Some(token.lease_until.as_str())
    {
        return Err(StoreError::RuntimeTaskConflict {
            task_id: token.task_id.to_string(),
        });
    }
    Ok(())
}

fn retry_at(now: &str, delay_seconds: i64) -> Result<String, StoreError> {
    let timestamp = OffsetDateTime::parse(now, &Rfc3339).map_err(|_| {
        crate::runtime::RuntimeError::InvalidTimestampFormat {
            value: now.to_owned(),
        }
    })?;
    Ok((timestamp + Duration::seconds(delay_seconds))
        .format(&Rfc3339)
        .map_err(|_| crate::runtime::RuntimeError::InvalidTimestampFormat {
            value: now.to_owned(),
        })?)
}

fn insert_run(
    transaction: &Transaction<'_>,
    state: &MutableState,
    events: &[Event],
) -> Result<(), StoreError> {
    let created_at = events
        .first()
        .map(|event| event.occurred_at.as_str())
        .unwrap_or_default();
    transaction.execute(
        "INSERT INTO runtime_runs (
             run_id, plan_revision, status, history_version,
             state_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            state.run_id.as_str(),
            &state.plan_revision,
            status_name(&state.status),
            state.history_version,
            serde_json::to_string(state)?,
            created_at,
        ],
    )?;
    for event in events {
        insert_event(transaction, state, event)?;
    }
    Ok(())
}

fn append_events(
    transaction: &Transaction<'_>,
    state: &MutableState,
    events: &[Event],
) -> Result<(), StoreError> {
    for event in events {
        insert_event(transaction, state, event)?;
    }
    Ok(())
}

fn apply_task_queue_changes(
    transaction: &Transaction<'_>,
    state: &MutableState,
    events: &[Event],
) -> Result<(), StoreError> {
    for event in events {
        match &event.kind {
            EventKind::TaskScheduled { task_id } => {
                let durable_task_id = format!("{}-{}-{}", state.run_id, task_id, event.seq);
                transaction.execute(
                    "INSERT INTO runtime_tasks (
                         durable_task_id, run_id, task_id, task_kind, status,
                         available_at, attempts, payload, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, 'agent_step', 'ready', ?4, 0, ?5, ?4, ?4)",
                    params![
                        durable_task_id,
                        state.run_id.as_str(),
                        task_id.as_str(),
                        &event.occurred_at,
                        serde_json::json!({ "task_id": task_id }).to_string(),
                    ],
                )?;
            }
            EventKind::TaskSucceeded {
                task_id,
                attempt_id,
                ..
            } => {
                update_leased_task(
                    transaction,
                    task_id,
                    attempt_id,
                    "completed",
                    &event.occurred_at,
                )?;
            }
            EventKind::TaskHandedOff {
                task_id,
                attempt_id,
                to_worker,
                ..
            } => {
                let changed = transaction.execute(
                    "UPDATE runtime_tasks
                     SET status = 'ready', lease_owner = NULL,
                         lease_until = NULL, preferred_worker = ?1,
                         updated_at = ?2
                     WHERE task_id = ?3 AND attempt_id = ?4 AND status = 'leased'",
                    params![
                        to_worker.as_ref().map(Actor::as_str),
                        &event.occurred_at,
                        task_id.as_str(),
                        attempt_id.as_str(),
                    ],
                )?;
                if changed != 1 {
                    return Err(StoreError::RuntimeTaskConflict {
                        task_id: task_id.to_string(),
                    });
                }
            }
            EventKind::TaskTimedOut {
                task_id,
                attempt_id,
                ..
            } => {
                let changed = transaction.execute(
                    "UPDATE runtime_tasks
                     SET status = 'failed', lease_owner = NULL,
                         lease_until = NULL, updated_at = ?1
                     WHERE task_id = ?2 AND attempt_id = ?3 AND status = 'leased'",
                    params![&event.occurred_at, task_id.as_str(), attempt_id.as_str()],
                )?;
                if changed != 1 {
                    return Err(StoreError::RuntimeTaskConflict {
                        task_id: task_id.to_string(),
                    });
                }
            }
            EventKind::TimerCreated {
                timer_id,
                task_id,
                fire_at,
            } => {
                transaction.execute(
                    "INSERT INTO runtime_timers (
                         timer_id, run_id, task_id, fire_at, timer_kind,
                         status, payload, created_at
                     ) VALUES (?1, ?2, ?3, ?4, 'retry', 'pending', ?5, ?6)",
                    params![
                        timer_id.as_str(),
                        state.run_id.as_str(),
                        task_id.as_str(),
                        fire_at,
                        serde_json::json!({ "task_id": task_id }).to_string(),
                        &event.occurred_at,
                    ],
                )?;
            }
            EventKind::TimerFired { timer_id } => {
                let changed = transaction.execute(
                    "UPDATE runtime_timers
                     SET status = 'fired', fired_at = ?1
                     WHERE timer_id = ?2 AND status = 'pending'",
                    params![&event.occurred_at, timer_id.as_str()],
                )?;
                if changed != 1 {
                    return Err(StoreError::RuntimeTimerConflict {
                        timer_id: timer_id.to_string(),
                    });
                }
            }
            EventKind::TaskFailed {
                task_id,
                attempt_id,
                ..
            } => {
                update_leased_task(
                    transaction,
                    task_id,
                    attempt_id,
                    "failed",
                    &event.occurred_at,
                )?;
            }
            EventKind::TaskWaiting {
                task_id,
                attempt_id,
                ..
            } => {
                update_leased_task(
                    transaction,
                    task_id,
                    attempt_id,
                    "failed",
                    &event.occurred_at,
                )?;
            }
            EventKind::TaskRetryableFailed {
                task_id,
                attempt_id,
                ..
            } => {
                update_leased_task(
                    transaction,
                    task_id,
                    attempt_id,
                    "failed",
                    &event.occurred_at,
                )?;
            }
            EventKind::TaskInvalidated { task_id, .. }
            | EventKind::TaskSuperseded { task_id, .. } => {
                transaction.execute(
                    "UPDATE runtime_tasks
                     SET status = 'failed', lease_owner = NULL,
                         lease_until = NULL, updated_at = ?1
                     WHERE run_id = ?2 AND task_id = ?3
                       AND status IN ('ready', 'leased')",
                    params![&event.occurred_at, state.run_id.as_str(), task_id.as_str()],
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn update_leased_task(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    attempt_id: &AttemptId,
    status: &str,
    updated_at: &str,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE runtime_tasks
         SET status = ?1, updated_at = ?2
         WHERE task_id = ?3 AND attempt_id = ?4 AND status = 'leased'",
        params![status, updated_at, task_id.as_str(), attempt_id.as_str()],
    )?;
    if changed != 1 {
        return Err(StoreError::RuntimeTaskConflict {
            task_id: task_id.to_string(),
        });
    }
    Ok(())
}

fn insert_event(
    transaction: &Transaction<'_>,
    state: &MutableState,
    event: &Event,
) -> Result<(), StoreError> {
    let metadata = event_parts(&event.kind)?;
    transaction.execute(
        "INSERT INTO execution_events (
             run_id, event_seq, event_id, event_type, plan_revision,
             task_id, attempt_id, actor, occurred_at, payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            state.run_id.as_str(),
            event.seq,
            &event.event_id,
            metadata.event_type,
            &state.plan_revision,
            metadata.task_id,
            metadata.attempt_id,
            metadata.actor,
            &event.occurred_at,
            serde_json::to_string(&metadata.payload)?,
        ],
    )?;
    Ok(())
}

fn update_state(
    transaction: &Transaction<'_>,
    state: &MutableState,
    events: &[Event],
) -> Result<(), StoreError> {
    let updated_at = events
        .last()
        .map(|event| event.occurred_at.as_str())
        .ok_or_else(|| StoreError::RuntimeRunNotFound {
            run_id: state.run_id.to_string(),
        })?;
    let previous_version = state
        .history_version
        .checked_sub(events.len() as u64)
        .ok_or_else(|| StoreError::RuntimeConflict {
            run_id: state.run_id.to_string(),
        })?;
    let changed = transaction.execute(
        "UPDATE runtime_runs
         SET plan_revision = ?1, status = ?2, history_version = ?3,
             state_json = ?4, updated_at = ?5
         WHERE run_id = ?6 AND history_version = ?7",
        params![
            &state.plan_revision,
            status_name(&state.status),
            state.history_version,
            serde_json::to_string(state)?,
            updated_at,
            state.run_id.as_str(),
            previous_version,
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::RuntimeConflict {
            run_id: state.run_id.to_string(),
        });
    }
    Ok(())
}

fn load_state(transaction: &Transaction<'_>, run_id: &RunId) -> Result<MutableState, StoreError> {
    let state_json: String = transaction
        .query_row(
            "SELECT state_json FROM runtime_runs WHERE run_id = ?1",
            params![run_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::RuntimeRunNotFound {
            run_id: run_id.to_string(),
        })?;
    Ok(serde_json::from_str(&state_json)?)
}

fn apply_events(state: &mut MutableState, events: &[Event]) -> Result<(), StoreError> {
    for event in events {
        crate::runtime::apply(state, event)?;
    }
    Ok(())
}

fn status_name(status: &crate::runtime::RunStatus) -> &'static str {
    match status {
        crate::runtime::RunStatus::Created => "created",
        crate::runtime::RunStatus::Running => "running",
        crate::runtime::RunStatus::Paused => "paused",
        crate::runtime::RunStatus::Completed => "completed",
        crate::runtime::RunStatus::Failed => "failed",
    }
}

fn event_parts(kind: &EventKind) -> Result<EventMetadata, StoreError> {
    let (event_type, task_id, attempt_id, actor) = match kind {
        EventKind::RunStarted { .. } => ("run_started", None, None, None),
        EventKind::TaskDiscovered { task_id } => {
            ("task_discovered", Some(task_id.to_string()), None, None)
        }
        EventKind::TaskReady { task_id } => ("task_ready", Some(task_id.to_string()), None, None),
        EventKind::TaskScheduled { task_id } => {
            ("task_scheduled", Some(task_id.to_string()), None, None)
        }
        EventKind::TaskClaimed {
            task_id,
            attempt_id,
            worker,
        } => (
            "task_claimed",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskStarted {
            task_id,
            attempt_id,
            worker,
        } => (
            "task_started",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskCheckpointed {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_checkpointed",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskSucceeded {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_succeeded",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskWaiting {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_waiting",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskHandedOff {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_handed_off",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskRetryableFailed {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_retryable_failed",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskFailed {
            task_id,
            attempt_id,
            worker,
            ..
        } => (
            "task_failed",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::TaskTimedOut {
            task_id,
            attempt_id,
            worker,
        } => (
            "task_timed_out",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            Some(worker.as_str().to_owned()),
        ),
        EventKind::RetryScheduled {
            task_id,
            attempt_id,
            ..
        } => (
            "retry_scheduled",
            Some(task_id.to_string()),
            Some(attempt_id.to_string()),
            None,
        ),
        EventKind::TimerCreated { task_id, .. } => {
            ("timer_created", Some(task_id.to_string()), None, None)
        }
        EventKind::TimerFired { .. } => ("timer_fired", None, None, None),
        EventKind::RunPaused => ("run_paused", None, None, None),
        EventKind::RunResumed => ("run_resumed", None, None, None),
        EventKind::PlanRevisionActivated { .. } => ("plan_revision_activated", None, None, None),
        EventKind::TaskInvalidated { task_id, .. } => {
            ("task_invalidated", Some(task_id.to_string()), None, None)
        }
        EventKind::TaskSuperseded { task_id, .. } => {
            ("task_superseded", Some(task_id.to_string()), None, None)
        }
    };
    Ok(EventMetadata {
        event_type,
        task_id,
        attempt_id,
        actor,
        payload: serde_json::to_value(kind)?,
    })
}

fn decode_event(run_id: &RunId, row: EventRow) -> Result<Event, StoreError> {
    let (
        seq,
        event_id,
        event_type,
        _plan_revision,
        _task_id,
        _attempt_id,
        _actor,
        occurred_at,
        payload,
    ) = row;
    let kind: EventKind = serde_json::from_str(&payload)?;
    let expected = event_type_for(&kind);
    if expected != event_type {
        return Err(StoreError::Runtime(
            crate::runtime::RuntimeError::InvalidEventType {
                expected,
                actual: event_type,
            },
        ));
    }
    Ok(Event {
        run_id: run_id.clone(),
        seq,
        event_id,
        occurred_at,
        kind,
    })
}

fn event_type_for(kind: &EventKind) -> String {
    match kind {
        EventKind::RunStarted { .. } => "run_started",
        EventKind::TaskDiscovered { .. } => "task_discovered",
        EventKind::TaskReady { .. } => "task_ready",
        EventKind::TaskScheduled { .. } => "task_scheduled",
        EventKind::TaskClaimed { .. } => "task_claimed",
        EventKind::TaskStarted { .. } => "task_started",
        EventKind::TaskCheckpointed { .. } => "task_checkpointed",
        EventKind::TaskSucceeded { .. } => "task_succeeded",
        EventKind::TaskFailed { .. } => "task_failed",
        EventKind::TaskWaiting { .. } => "task_waiting",
        EventKind::TaskHandedOff { .. } => "task_handed_off",
        EventKind::TaskRetryableFailed { .. } => "task_retryable_failed",
        EventKind::TaskTimedOut { .. } => "task_timed_out",
        EventKind::RetryScheduled { .. } => "retry_scheduled",
        EventKind::TimerCreated { .. } => "timer_created",
        EventKind::TimerFired { .. } => "timer_fired",
        EventKind::RunPaused => "run_paused",
        EventKind::RunResumed => "run_resumed",
        EventKind::PlanRevisionActivated { .. } => "plan_revision_activated",
        EventKind::TaskInvalidated { .. } => "task_invalidated",
        EventKind::TaskSuperseded { .. } => "task_superseded",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Store;
    use crate::{
        domain::Actor,
        plan::{PlanProposal, ProposalStatus},
        runtime::{AttemptId, Command, CommandKind, RunId, TaskId},
    };

    fn command(request_id: &str, kind: CommandKind) -> Command {
        Command::new(request_id, kind).expect("command")
    }

    fn run_id() -> RunId {
        RunId::new("run-persisted").expect("run id")
    }

    fn task_id() -> TaskId {
        TaskId::new("task-persisted").expect("task id")
    }

    fn attempt_id() -> AttemptId {
        AttemptId::new("attempt-persisted-1").expect("attempt id")
    }

    fn start_command() -> Command {
        command(
            "start-run",
            CommandKind::StartRun {
                now: "2026-09-29T00:00:00Z".to_owned(),
            },
        )
    }

    fn setup_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    #[test]
    fn start_and_mutate_run_are_persisted_as_one_history() {
        let mut store = setup_store();
        let started = store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        assert_eq!(started.state.history_version, 1);

        let discovered = store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");
        assert_eq!(discovered.events.len(), 2);
        assert_eq!(discovered.state.history_version, 3);

        let state = store.runtime_state(&run_id()).expect("runtime state");
        assert_eq!(state, discovered.state);

        let page = store
            .runtime_events(&run_id(), 0, 10)
            .expect("runtime events");
        assert_eq!(page.head, 3);
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.events[1].run_id, run_id());
        assert_eq!(page.events[2].event_id, "discover-task-1");
    }

    #[test]
    fn invalid_command_does_not_append_history_or_change_snapshot() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");

        let error = store
            .apply_runtime_command(
                &run_id(),
                command(
                    "complete-before-discover",
                    CommandKind::CompleteTask {
                        task_id: task_id(),
                        attempt_id: attempt_id(),
                        worker: Actor::new("worker-a").expect("actor"),
                        result: json!({ "ok": true }),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect_err("invalid command must fail");
        assert!(error.to_string().contains("task"));

        let state = store.runtime_state(&run_id()).expect("runtime state");
        assert_eq!(state.history_version, 1);
        let page = store
            .runtime_events(&run_id(), 0, 10)
            .expect("runtime events");
        assert_eq!(page.head, 1);
        assert_eq!(page.events.len(), 1);
    }

    #[test]
    fn plan_revision_activation_updates_materialized_revision() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .create_plan_proposal(&PlanProposal {
                proposal_id: "runtime-proposal".to_owned(),
                run_id: run_id().to_string(),
                base_revision: "plan-1".to_owned(),
                head_revision: "plan-2".to_owned(),
                author: "agent-a".to_owned(),
                reason: "activate next plan".to_owned(),
                status: ProposalStatus::Proposed,
                created_at: "2026-09-29T00:00:00Z".to_owned(),
                approved_by: None,
                approved_at: None,
            })
            .expect("proposal");
        store
            .approve_plan_proposal("runtime-proposal", "reviewer", "2026-09-29T00:00:00Z")
            .expect("approve proposal");
        let result = store
            .apply_runtime_command(
                &run_id(),
                command(
                    "reconcile-plan",
                    CommandKind::ReconcilePlan {
                        new_plan_revision: "plan-2".to_owned(),
                        discovered: vec![task_id()],
                        ready: vec![task_id()],
                        invalidated: Vec::new(),
                        superseded: Vec::new(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("reconcile plan");

        assert_eq!(result.state.plan_revision, "plan-2");
        let revision: String = store
            .connection
            .query_row(
                "SELECT plan_revision FROM runtime_runs WHERE run_id = ?1",
                rusqlite::params![run_id().as_str()],
                |row| row.get(0),
            )
            .expect("materialized revision");
        assert_eq!(revision, "plan-2");
        assert!(store.verify_runtime_state(&run_id()).expect("verify").valid);
    }

    #[test]
    fn direct_runtime_revision_change_requires_approved_proposal() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        let error = store
            .apply_runtime_command(
                &run_id(),
                command(
                    "unapproved-reconcile",
                    CommandKind::ReconcilePlan {
                        new_plan_revision: "plan-unapproved".to_owned(),
                        discovered: Vec::new(),
                        ready: Vec::new(),
                        invalidated: Vec::new(),
                        superseded: Vec::new(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect_err("unapproved revision must fail");
        assert!(matches!(
            error,
            super::super::StoreError::PlanRevisionNotApproved { .. }
        ));
        assert_eq!(
            store.runtime_state(&run_id()).expect("state").plan_revision,
            "plan-1"
        );
    }

    #[test]
    fn reconciliation_populates_the_durable_worker_queue() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "initial-reconcile",
                    CommandKind::ReconcilePlan {
                        new_plan_revision: "plan-1".to_owned(),
                        discovered: vec![task_id()],
                        ready: vec![task_id()],
                        invalidated: Vec::new(),
                        superseded: Vec::new(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("reconcile");

        let (lease, _) = store
            .poll_runtime_task(
                Actor::new("worker-a").expect("actor"),
                "2026-09-29T00:00:02Z",
                "2026-09-29T00:01:02Z",
            )
            .expect("poll reconciled task");
        let token = store.issue_runtime_task_token(&lease).expect("token");
        store
            .start_runtime_task(&token, "2026-09-29T00:00:03Z")
            .expect("start");
        let completed = store
            .complete_runtime_task(&token, json!({ "ok": true }), "2026-09-29T00:00:04Z")
            .expect("complete");
        assert_eq!(
            completed.state.task(&task_id()).expect("task").status,
            crate::runtime::TaskStatus::Succeeded
        );
    }

    #[test]
    fn event_page_respects_cursor_and_limit() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");

        let page = store
            .runtime_events(&run_id(), 1, 1)
            .expect("runtime events");
        assert_eq!(page.head, 3);
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].seq, 2);
    }

    #[test]
    fn scheduling_and_polling_are_transactionally_connected() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "schedule-task",
                    CommandKind::ScheduleTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:02Z".to_owned(),
                    },
                ),
            )
            .expect("schedule task");

        let (lease, result) = store
            .poll_runtime_task(
                Actor::new("worker-a").expect("actor"),
                "2026-09-29T00:00:03Z",
                "2026-09-29T00:01:03Z",
            )
            .expect("poll task");
        assert_eq!(lease.task_id, task_id());
        assert_eq!(lease.worker.as_str(), "worker-a");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.state.task(&task_id()).expect("task").attempt, 1);
        let token = store
            .issue_runtime_task_token(&lease)
            .expect("issue runtime token");

        store
            .start_runtime_task(&token, "2026-09-29T00:00:04Z")
            .expect("start attempt");
        store
            .complete_runtime_task(&token, json!({ "ok": true }), "2026-09-29T00:00:05Z")
            .expect("complete task");
        assert!(matches!(
            store.complete_runtime_task(&token, json!({ "ok": true }), "2026-09-29T00:00:06Z",),
            Err(super::StoreError::RuntimeTaskConflict { .. })
        ));

        assert!(matches!(
            store.poll_runtime_task(
                Actor::new("worker-b").expect("actor"),
                "2026-09-29T00:00:06Z",
                "2026-09-29T00:01:06Z",
            ),
            Err(super::StoreError::NoRuntimeTask)
        ));
    }

    #[test]
    fn heartbeat_timeout_and_timer_retry_are_durable() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "schedule-task",
                    CommandKind::ScheduleTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:02Z".to_owned(),
                    },
                ),
            )
            .expect("schedule task");
        let (lease, _) = store
            .poll_runtime_task(
                Actor::new("worker-a").expect("actor"),
                "2026-09-29T00:00:03Z",
                "2026-09-29T00:01:03Z",
            )
            .expect("poll task");
        let token = store.issue_runtime_task_token(&lease).expect("issue token");
        store
            .start_runtime_task(&token, "2026-09-29T00:00:04Z")
            .expect("start attempt");

        let extended = store
            .heartbeat_runtime_task(&token, "2026-09-29T00:00:30Z", "2026-09-29T00:02:00Z")
            .expect("heartbeat");
        assert_eq!(extended.lease_until, "2026-09-29T00:02:00Z");

        let new_token = store
            .issue_runtime_task_token(&extended)
            .expect("issue extended token");
        let error = store
            .complete_runtime_task(&token, json!({ "stale": true }), "2026-09-29T00:00:31Z")
            .expect_err("old heartbeat token must be fenced");
        assert!(matches!(
            error,
            super::StoreError::RuntimeTaskConflict { .. }
        ));

        store
            .timeout_runtime_task(
                &lease.durable_task_id,
                crate::runtime::TimerId::new("retry-timer-1").expect("timer"),
                "2026-09-29T00:02:10Z",
                "2026-09-29T00:02:01Z",
            )
            .expect("timeout");
        let waiting = store.runtime_state(&run_id()).expect("state");
        assert_eq!(
            waiting.task(&task_id()).expect("task").status,
            crate::runtime::TaskStatus::Waiting
        );

        store
            .fire_runtime_timer(
                crate::runtime::TimerId::new("retry-timer-1").expect("timer"),
                "2026-09-29T00:02:11Z",
            )
            .expect("fire timer");
        let (retry, _) = store
            .poll_runtime_task(
                Actor::new("worker-b").expect("actor"),
                "2026-09-29T00:02:12Z",
                "2026-09-29T00:03:12Z",
            )
            .expect("poll retry");
        assert_ne!(retry.durable_task_id, lease.durable_task_id);
        assert_ne!(retry.attempt_id, lease.attempt_id);

        let completed = store
            .complete_runtime_task(
                &new_token,
                json!({ "stale": false }),
                "2026-09-29T00:02:13Z",
            )
            .expect_err("old worker token must not complete retry");
        assert!(matches!(
            completed,
            super::StoreError::RuntimeTaskConflict { .. }
        ));
    }

    #[test]
    fn history_rebuild_matches_materialized_state() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");

        let rebuilt = store.rebuild_runtime_state(&run_id()).expect("rebuild");
        let materialized = store.runtime_state(&run_id()).expect("state");
        assert_eq!(rebuilt, materialized);
        let verification = store.verify_runtime_state(&run_id()).expect("verify");
        assert!(verification.valid);
        assert_eq!(verification.history_version, 3);
        assert_eq!(verification.event_count, 3);
    }

    #[test]
    fn verify_detects_snapshot_divergence_but_rebuild_remains_authoritative() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .connection
            .execute(
                "UPDATE runtime_runs SET state_json = ?1 WHERE run_id = ?2",
                rusqlite::params!["{\"corrupted\":true}", run_id().as_str()],
            )
            .expect("corrupt snapshot");

        let verification = store.verify_runtime_state(&run_id()).expect("verify");
        assert!(!verification.valid);
        let rebuilt = store.rebuild_runtime_state(&run_id()).expect("rebuild");
        assert_eq!(rebuilt.history_version, 1);
        assert_eq!(rebuilt.status, crate::runtime::RunStatus::Running);
    }

    #[test]
    fn recovery_reclaims_a_claimed_task_after_lease_expiry() {
        let mut store = setup_store();
        store
            .start_run(run_id(), "plan-1", start_command())
            .expect("start run");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "discover-task",
                    CommandKind::DiscoverTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:01Z".to_owned(),
                    },
                ),
            )
            .expect("discover task");
        store
            .apply_runtime_command(
                &run_id(),
                command(
                    "schedule-task",
                    CommandKind::ScheduleTask {
                        task_id: task_id(),
                        now: "2026-09-29T00:00:02Z".to_owned(),
                    },
                ),
            )
            .expect("schedule task");
        let (lease, _) = store
            .poll_runtime_task(
                Actor::new("worker-a").expect("actor"),
                "2026-09-29T00:00:03Z",
                "2026-09-29T00:00:05Z",
            )
            .expect("poll task");

        let report = store
            .recover_expired_runtime_tasks(&run_id(), "2026-09-29T00:00:06Z", 10)
            .expect("recover");
        assert_eq!(report.inspected, 1);
        assert_eq!(report.recovered, vec![lease.durable_task_id.clone()]);
        assert_eq!(
            report.state.task(&task_id()).expect("task").status,
            crate::runtime::TaskStatus::Waiting
        );

        store
            .fire_runtime_timer(
                crate::runtime::TimerId::new(format!("timer-{}", lease.durable_task_id))
                    .expect("timer"),
                "2026-09-29T00:00:16Z",
            )
            .expect("fire retry timer");
        let (retry, _) = store
            .poll_runtime_task(
                Actor::new("worker-b").expect("actor"),
                "2026-09-29T00:00:17Z",
                "2026-09-29T00:01:17Z",
            )
            .expect("poll retry");
        assert_ne!(retry.durable_task_id, lease.durable_task_id);
    }
}
