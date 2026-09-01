use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::{Actor, IssueId, Operation, Revision, Status};

use super::{StoreError, issues::get_issue_from};

const MAX_FIELD_BYTES: usize = 4_000;

type CheckpointRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    u64,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub issue_id: IssueId,
    pub actor: Actor,
    pub summary: String,
    pub completed: String,
    pub next_step: String,
    pub blockers: String,
    pub evidence: String,
    pub created_at: String,
    pub revision: Revision,
}

impl super::Store {
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_checkpoint(
        &mut self,
        issue_id: &IssueId,
        actor: Actor,
        summary: &str,
        completed: &str,
        next_step: &str,
        blockers: &str,
        evidence: &str,
        created_at: &str,
    ) -> Result<Checkpoint, StoreError> {
        validate_text("summary", summary, true)?;
        validate_text("next_step", next_step, true)?;
        validate_text("completed", completed, false)?;
        validate_text("blockers", blockers, false)?;
        validate_text("evidence", evidence, false)?;
        if created_at.trim().is_empty() {
            return Err(StoreError::InvalidCheckpoint {
                message: "created_at must not be empty".to_owned(),
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue =
            get_issue_from(&transaction, issue_id)?.ok_or_else(|| StoreError::IssueNotFound {
                id: issue_id.to_string(),
            })?;
        if issue.status != Status::InProgress || issue.assignee.as_ref() != Some(&actor) {
            return Err(StoreError::CheckpointNotOwned {
                id: issue_id.to_string(),
            });
        }

        let previous: Option<u64> = transaction
            .query_row(
                "SELECT revision FROM session_checkpoints WHERE issue_id = ?1",
                params![issue_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let checkpoint = Checkpoint {
            issue_id: issue_id.clone(),
            actor,
            summary: summary.to_owned(),
            completed: completed.to_owned(),
            next_step: next_step.to_owned(),
            blockers: blockers.to_owned(),
            evidence: evidence.to_owned(),
            created_at: created_at.to_owned(),
            revision: Revision::new(previous.unwrap_or(0).saturating_add(1)),
        };

        upsert_checkpoint_row(&transaction, &checkpoint)?;
        append_checkpoint_event(&transaction, &checkpoint)?;
        transaction.commit()?;
        Ok(checkpoint)
    }

    pub fn get_checkpoint(&self, issue_id: &IssueId) -> Result<Option<Checkpoint>, StoreError> {
        get_checkpoint_from(&self.connection, issue_id)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<Checkpoint>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT issue_id, actor, summary, completed, next_step,
                    blockers, evidence, created_at, revision
             FROM session_checkpoints
             ORDER BY created_at, issue_id",
        )?;
        let rows = statement.query_map([], decode_row)?;
        rows.map(|row| row.map_err(StoreError::from).and_then(decode_checkpoint))
            .collect()
    }
}

fn validate_text(field: &'static str, value: &str, required: bool) -> Result<(), StoreError> {
    if required && value.trim().is_empty() {
        return Err(StoreError::InvalidCheckpoint {
            message: format!("{field} must not be empty"),
        });
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(StoreError::InvalidCheckpoint {
            message: format!("{field} exceeds {MAX_FIELD_BYTES} bytes"),
        });
    }
    Ok(())
}

fn get_checkpoint_from(
    connection: &Connection,
    issue_id: &IssueId,
) -> Result<Option<Checkpoint>, StoreError> {
    let row = connection
        .query_row(
            "SELECT issue_id, actor, summary, completed, next_step,
                    blockers, evidence, created_at, revision
             FROM session_checkpoints
             WHERE issue_id = ?1",
            params![issue_id.as_str()],
            decode_row,
        )
        .optional()?;
    row.map(decode_checkpoint).transpose()
}

fn decode_row(row: &rusqlite::Row<'_>) -> Result<CheckpointRow, rusqlite::Error> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn decode_checkpoint(
    row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        u64,
    ),
) -> Result<Checkpoint, StoreError> {
    Ok(Checkpoint {
        issue_id: IssueId::new(row.0)?,
        actor: Actor::new(row.1)?,
        summary: row.2,
        completed: row.3,
        next_step: row.4,
        blockers: row.5,
        evidence: row.6,
        created_at: row.7,
        revision: Revision::new(row.8),
    })
}

fn upsert_checkpoint_row(
    transaction: &Transaction<'_>,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO session_checkpoints
            (issue_id, actor, summary, completed, next_step,
             blockers, evidence, created_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(issue_id) DO UPDATE SET
            actor = excluded.actor,
            summary = excluded.summary,
            completed = excluded.completed,
            next_step = excluded.next_step,
            blockers = excluded.blockers,
            evidence = excluded.evidence,
            created_at = excluded.created_at,
            revision = excluded.revision",
        params![
            checkpoint.issue_id.as_str(),
            checkpoint.actor.as_str(),
            &checkpoint.summary,
            &checkpoint.completed,
            &checkpoint.next_step,
            &checkpoint.blockers,
            &checkpoint.evidence,
            &checkpoint.created_at,
            checkpoint.revision.value(),
        ],
    )?;
    Ok(())
}

fn append_checkpoint_event(
    transaction: &Transaction<'_>,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    super::journal::append_event(
        transaction,
        Operation::CheckpointUpdated,
        Some(&checkpoint.issue_id),
        Some(&checkpoint.actor),
        &checkpoint.created_at,
        Some(serde_json::to_value(checkpoint)?),
        serde_json::json!({ "revision": checkpoint.revision.value() }),
    )
}

pub(crate) fn checkpoint_snapshot(value: serde_json::Value) -> Result<Checkpoint, StoreError> {
    let checkpoint: Checkpoint = serde_json::from_value(value)?;
    validate_text("summary", &checkpoint.summary, true)?;
    validate_text("next_step", &checkpoint.next_step, true)?;
    validate_text("completed", &checkpoint.completed, false)?;
    validate_text("blockers", &checkpoint.blockers, false)?;
    validate_text("evidence", &checkpoint.evidence, false)?;
    if checkpoint.created_at.trim().is_empty() || checkpoint.revision.value() == 0 {
        return Err(StoreError::InvalidCheckpoint {
            message: "checkpoint timestamp and revision are required".to_owned(),
        });
    }
    Ok(checkpoint)
}

pub(crate) fn replay_checkpoint(
    transaction: &Transaction<'_>,
    event: &crate::store::JournalEvent,
) -> Result<(), StoreError> {
    let checkpoint =
        checkpoint_snapshot(
            event
                .snapshot
                .clone()
                .ok_or_else(|| StoreError::ReplayConflict {
                    message: format!("event `{}` is missing checkpoint snapshot", event.event_id),
                })?,
        )?;
    if event.issue_id.as_ref() != Some(&checkpoint.issue_id)
        || event.actor.as_ref() != Some(&checkpoint.actor)
    {
        return Err(StoreError::ReplayConflict {
            message: format!(
                "event `{}` has inconsistent checkpoint owner",
                event.event_id
            ),
        });
    }
    let issue = get_issue_from(transaction, &checkpoint.issue_id)?.ok_or_else(|| {
        StoreError::ReplayConflict {
            message: "checkpoint issue predecessor is missing".to_owned(),
        }
    })?;
    if issue.assignee.as_ref() != Some(&checkpoint.actor) || issue.status != Status::InProgress {
        return Err(StoreError::ReplayConflict {
            message: "checkpoint owner is not the current issue owner".to_owned(),
        });
    }
    let previous: Option<u64> = transaction
        .query_row(
            "SELECT revision FROM session_checkpoints WHERE issue_id = ?1",
            params![checkpoint.issue_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let expected = previous.unwrap_or(0).saturating_add(1);
    if checkpoint.revision.value() != expected {
        return Err(StoreError::ReplayConflict {
            message: format!("checkpoint revision predecessor is missing: expected {expected}"),
        });
    }
    upsert_checkpoint_row(transaction, &checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{IssueDraft, Priority},
        store::Store,
    };
    use tempfile::tempdir;

    #[test]
    fn checkpoint_requires_the_current_owner() {
        let directory = tempdir().expect("tempdir");
        let mut store = Store::open(&directory.path().join("state.db")).expect("open");
        store.migrate().expect("migrate");
        let issue = store
            .insert_draft(
                IssueId::new("ag-abcdef").expect("id"),
                IssueDraft {
                    title: "task".to_owned(),
                    description: String::new(),
                    priority: Priority::new(1).expect("priority"),
                    kind: crate::domain::IssueKind::Task,
                    created_at: "2026-09-26T00:00:00Z".to_owned(),
                },
            )
            .expect("insert");
        let actor = Actor::new("agent-a").expect("actor");
        store
            .claim_issue(&issue.id, actor.clone(), "2026-09-26T00:01:00Z", None)
            .expect("claim");
        let checkpoint = store
            .upsert_checkpoint(
                &issue.id,
                actor,
                "started",
                "none",
                "continue",
                "",
                "cargo test",
                "2026-09-26T00:02:00Z",
            )
            .expect("checkpoint");
        assert_eq!(checkpoint.revision.value(), 1);
        assert_eq!(
            store
                .get_checkpoint(&issue.id)
                .expect("read")
                .expect("checkpoint")
                .next_step,
            "continue"
        );
    }
}
