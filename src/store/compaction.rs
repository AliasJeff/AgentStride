use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::{Actor, Issue, IssueId, Operation};

use super::{Store, StoreError, issues::get_issue_from};

const MAX_SUMMARY_BYTES: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionCandidate {
    pub issue: Issue,
    pub content_bytes: usize,
    pub active_snapshot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompactionMetadata {
    pub actor: String,
    pub model: String,
    #[serde(default)]
    pub provider: String,
    pub original_bytes: usize,
    pub compacted_bytes: usize,
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OriginalContent {
    title: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CompactionPayload {
    level: u64,
    original: OriginalContent,
    #[serde(default)]
    metadata: CompactionMetadata,
}

#[derive(Debug, Deserialize)]
struct RestorePayload {
    level: u64,
}

impl Store {
    pub fn list_compaction_candidates(
        &self,
        issue_id: Option<&IssueId>,
        min_bytes: usize,
    ) -> Result<Vec<CompactionCandidate>, StoreError> {
        let issues = if let Some(issue_id) = issue_id {
            self.get_issue(issue_id)?.into_iter().collect::<Vec<_>>()
        } else {
            self.list_issues()?
        };

        issues
            .into_iter()
            .filter(|issue| issue.status == crate::domain::Status::Closed)
            .filter(|issue| issue.description.len() >= min_bytes)
            .map(|issue| {
                let active_snapshot = has_active_snapshot(&self.connection, &issue.id)?;
                Ok(CompactionCandidate {
                    content_bytes: issue.description.len(),
                    issue,
                    active_snapshot,
                })
            })
            .filter_map(|candidate| match candidate {
                Ok(candidate) if !candidate.active_snapshot => Some(Ok(candidate)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub fn compact_issue(
        &mut self,
        id: &IssueId,
        summary: &str,
        now: &str,
    ) -> Result<Issue, StoreError> {
        self.compact_issue_with_metadata(id, summary, now, CompactionMetadata::default())
    }

    pub fn compact_issue_with_metadata(
        &mut self,
        id: &IssueId,
        summary: &str,
        now: &str,
        mut metadata: CompactionMetadata,
    ) -> Result<Issue, StoreError> {
        validate_summary(summary, now)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        if has_active_snapshot_tx(&transaction, id)? {
            return Err(StoreError::InvalidCompaction {
                message: "issue already has an active compaction snapshot".to_owned(),
            });
        }
        let original_bytes = issue.description.len();
        let compacted_bytes = summary.len();
        if compacted_bytes >= original_bytes {
            return Err(StoreError::InvalidCompaction {
                message: format!(
                    "summary must be shorter than original description ({compacted_bytes} >= {original_bytes} bytes)"
                ),
            });
        }
        metadata.original_bytes = original_bytes;
        metadata.compacted_bytes = compacted_bytes;
        let event_actor = if metadata.actor.trim().is_empty() {
            None
        } else {
            Some(Actor::new(metadata.actor.clone())?)
        };
        let level: u64 = transaction.query_row(
            "SELECT COALESCE(MAX(compaction_level), 0) + 1
             FROM issue_snapshots WHERE issue_id = ?1",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        let payload = CompactionPayload {
            level,
            original: OriginalContent {
                title: issue.title.clone(),
                description: issue.description.clone(),
            },
            metadata,
        };
        transaction.execute(
            "INSERT INTO issue_snapshots
                (issue_id, compaction_level, content_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.as_str(),
                level,
                serde_json::to_string(&payload.original)?,
                now
            ],
        )?;
        let mut compacted = issue;
        compacted.description = summary.to_owned();
        compacted.updated_at = now.to_owned();
        compacted.revision = compacted.revision.next();
        update_issue_row(&transaction, &compacted)?;
        super::journal::append_event(
            &transaction,
            Operation::IssueCompacted,
            Some(id),
            event_actor.as_ref(),
            now,
            Some(super::journal::snapshot(&compacted)?),
            serde_json::to_value(payload)?,
        )?;
        transaction.commit()?;
        Ok(compacted)
    }

    pub fn restore_issue(&mut self, id: &IssueId, now: &str) -> Result<Issue, StoreError> {
        if now.trim().is_empty() {
            return Err(StoreError::InvalidCompaction {
                message: "timestamp must not be empty".to_owned(),
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        let snapshot: (u64, String) = transaction
            .query_row(
                "SELECT compaction_level, content_json
                 FROM issue_snapshots
                 WHERE issue_id = ?1 AND restored_at IS NULL
                 ORDER BY compaction_level DESC LIMIT 1",
                params![id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NoSnapshot { id: id.to_string() })?;
        let original: OriginalContent = serde_json::from_str(&snapshot.1)?;
        let mut restored = issue;
        restored.title = original.title;
        restored.description = original.description;
        restored.updated_at = now.to_owned();
        restored.revision = restored.revision.next();
        update_issue_row(&transaction, &restored)?;
        transaction.execute(
            "UPDATE issue_snapshots SET restored_at = ?1
             WHERE issue_id = ?2 AND compaction_level = ?3",
            params![now, id.as_str(), snapshot.0],
        )?;
        super::journal::append_event(
            &transaction,
            Operation::IssueRestored,
            Some(id),
            None,
            now,
            Some(super::journal::snapshot(&restored)?),
            serde_json::json!({ "level": snapshot.0 }),
        )?;
        transaction.commit()?;
        Ok(restored)
    }
}

fn has_active_snapshot(
    connection: &rusqlite::Connection,
    id: &IssueId,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM issue_snapshots
            WHERE issue_id = ?1 AND restored_at IS NULL
        )",
        rusqlite::params![id.as_str()],
        |row| row.get(0),
    )?)
}

fn has_active_snapshot_tx(
    connection: &rusqlite::Transaction<'_>,
    id: &IssueId,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM issue_snapshots
            WHERE issue_id = ?1 AND restored_at IS NULL
        )",
        rusqlite::params![id.as_str()],
        |row| row.get(0),
    )?)
}

fn validate_summary(summary: &str, now: &str) -> Result<(), StoreError> {
    if summary.trim().is_empty() || now.trim().is_empty() {
        return Err(StoreError::InvalidCompaction {
            message: "summary and timestamp are required".to_owned(),
        });
    }
    if summary.len() > MAX_SUMMARY_BYTES {
        return Err(StoreError::InvalidCompaction {
            message: format!("summary exceeds {MAX_SUMMARY_BYTES} bytes"),
        });
    }
    Ok(())
}

fn update_issue_row(
    transaction: &rusqlite::Transaction<'_>,
    issue: &Issue,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE issues
         SET title = ?1, description = ?2, updated_at = ?3, revision = ?4
         WHERE id = ?5 AND revision = ?6",
        params![
            &issue.title,
            &issue.description,
            &issue.updated_at,
            issue.revision.value(),
            issue.id.as_str(),
            issue.revision.value().saturating_sub(1)
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::RevisionMismatch {
            expected: issue.revision.value().saturating_sub(1),
        });
    }
    Ok(())
}

pub(crate) fn replay_compaction(
    transaction: &rusqlite::Transaction<'_>,
    event: &crate::store::JournalEvent,
) -> Result<(), StoreError> {
    let issue = super::journal::issue_snapshot_for_replay(event)?;
    match event.operation {
        Operation::IssueCompacted => {
            let payload: CompactionPayload = serde_json::from_value(event.payload.clone())?;
            transaction.execute(
                "INSERT INTO issue_snapshots
                    (issue_id, compaction_level, content_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    issue.id.as_str(),
                    payload.level,
                    serde_json::to_string(&payload.original)?,
                    event.created_at
                ],
            )?;
        }
        Operation::IssueRestored => {
            let payload: RestorePayload = serde_json::from_value(event.payload.clone())?;
            transaction.execute(
                "UPDATE issue_snapshots SET restored_at = ?1
                 WHERE issue_id = ?2 AND compaction_level = ?3",
                params![event.created_at, issue.id.as_str(), payload.level],
            )?;
        }
        _ => unreachable!("replay_compaction only receives compaction operations"),
    }
    Ok(())
}
