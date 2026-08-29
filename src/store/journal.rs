use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::{Actor, Edge, EdgeType, Issue, IssueId, Operation, Status, generate_digest},
    graph::would_create_cycle,
};

use super::{Store, StoreError, edges::load_edges, issues::get_issue_from};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub seq: u64,
    pub event_id: String,
    pub operation: Operation,
    pub issue_id: Option<IssueId>,
    pub actor: Option<Actor>,
    pub created_at: String,
    pub snapshot: Option<Value>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalPage {
    pub events: Vec<JournalEvent>,
    pub head: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotImportResult {
    pub issues: usize,
    pub edges: usize,
}

impl super::Store {
    pub fn list_events(&self, since: u64, limit: usize) -> Result<JournalPage, StoreError> {
        let head = journal_head(&self.connection)?;
        if limit == 0 {
            return Ok(JournalPage {
                events: Vec::new(),
                head,
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT seq, event_id, operation, issue_id, actor,
                    snapshot, payload, created_at
             FROM journal
             WHERE seq > ?1
             ORDER BY seq
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![since, limit as u64], decode_row)?;
        let events = rows
            .map(|row| row.map_err(StoreError::from).and_then(decode_event))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(JournalPage { events, head })
    }

    pub fn list_recent_events(&self, limit: usize) -> Result<Vec<JournalEvent>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT seq, event_id, operation, issue_id, actor,
                    snapshot, payload, created_at
             FROM journal
             ORDER BY seq DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as u64], decode_row)?;
        let mut events = rows
            .map(|row| row.map_err(StoreError::from).and_then(decode_event))
            .collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(events)
    }

    pub fn replay_event(&mut self, event: &JournalEvent) -> Result<bool, StoreError> {
        validate_event(event)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT seq, event_id, operation, issue_id, actor,
                        snapshot, payload, created_at
                 FROM journal
                 WHERE event_id = ?1",
                params![&event.event_id],
                decode_row,
            )
            .optional()?;
        if let Some(row) = existing {
            let existing = decode_event(row)?;
            if same_event(&existing, event) {
                transaction.commit()?;
                return Ok(false);
            }
            return Err(replay_conflict(
                event,
                "event_id already exists with different content",
            ));
        }

        match event.operation {
            Operation::IssueCreated
            | Operation::IssueUpdated
            | Operation::IssueClaimed
            | Operation::IssueClosed => replay_issue(&transaction, event)?,
            Operation::EdgeCreated | Operation::EdgeRemoved => replay_edge(&transaction, event)?,
            Operation::MemoryCreated | Operation::MemoryUpdated | Operation::MemoryDeleted => {
                replay_memory(&transaction, event)?
            }
            Operation::CheckpointUpdated => {
                super::checkpoints::replay_checkpoint(&transaction, event)?
            }
            Operation::HandoffCreated => replay_handoff(&transaction, event)?,
            Operation::MessageSent => {
                let message: super::Message = serde_json::from_value(
                    event
                        .snapshot
                        .clone()
                        .ok_or_else(|| replay_conflict(event, "missing message snapshot"))?,
                )?;
                Store::replay_message(&transaction, &message)?;
            }
            Operation::MessageRead => {
                Store::replay_message_read(&transaction, event)?;
            }
            Operation::IssueCompacted | Operation::IssueRestored => {
                replay_issue(&transaction, event)?;
                super::compaction::replay_compaction(&transaction, event)?;
            }
        }
        if matches!(
            event.operation,
            Operation::IssueClaimed | Operation::IssueUpdated | Operation::IssueClosed
        ) {
            replay_lease_payload(&transaction, event)?;
        }
        insert_replayed_event(&transaction, event)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn import_snapshot<I, E>(
        &mut self,
        issues: I,
        edges: E,
    ) -> Result<SnapshotImportResult, StoreError>
    where
        I: IntoIterator<Item = Result<Issue, StoreError>>,
        E: IntoIterator<Item = Result<Edge, StoreError>>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut issue_count = 0;
        for issue in issues {
            let issue = issue?;
            issue.validate()?;
            if get_issue_from(&transaction, &issue.id)?.is_some() {
                return Err(StoreError::ReplayConflict {
                    message: format!("snapshot issue `{}` already exists", issue.id),
                });
            }
            insert_issue_snapshot(&transaction, &issue)?;
            issue_count += 1;
        }

        let mut edge_count = 0;
        for edge in edges {
            let edge = edge?;
            if !issue_exists(&transaction, &edge.source_id)?
                || !issue_exists(&transaction, &edge.target_id)?
            {
                return Err(StoreError::ReplayConflict {
                    message: format!(
                        "snapshot edge `{}` -> `{}` has a missing endpoint",
                        edge.source_id, edge.target_id
                    ),
                });
            }
            if edge_pair_exists(&transaction, &edge)? {
                return Err(StoreError::ReplayConflict {
                    message: format!(
                        "snapshot edge `{}` -> `{}` already exists",
                        edge.source_id, edge.target_id
                    ),
                });
            }
            if edge.edge_type.is_scheduling() {
                let existing = load_edges(&transaction)?;
                would_create_cycle(&edge.source_id, &edge.target_id, existing)?;
            }
            insert_edge_snapshot(&transaction, &edge)?;
            edge_count += 1;
        }

        transaction.commit()?;
        Ok(SnapshotImportResult {
            issues: issue_count,
            edges: edge_count,
        })
    }
}

fn validate_event(event: &JournalEvent) -> Result<(), StoreError> {
    if event.event_id.trim().is_empty() {
        return Err(StoreError::ReplayConflict {
            message: "event_id must not be empty".to_owned(),
        });
    }
    if event.created_at.trim().is_empty() {
        return Err(StoreError::ReplayConflict {
            message: format!("event `{}` has an empty created_at", event.event_id),
        });
    }
    match event.operation {
        Operation::IssueCreated
        | Operation::IssueUpdated
        | Operation::IssueClaimed
        | Operation::IssueClosed => {
            let issue = issue_snapshot(event)?;
            if event.issue_id.as_ref() != Some(&issue.id) {
                return Err(replay_conflict(event, "issue_id does not match snapshot"));
            }
            if event.operation == Operation::IssueClaimed
                && (issue.status != Status::InProgress
                    || issue.assignee.as_ref() != event.actor.as_ref())
            {
                return Err(replay_conflict(event, "invalid claim snapshot"));
            }
            if event.operation == Operation::IssueClosed && issue.status != Status::Closed {
                return Err(replay_conflict(event, "invalid close snapshot"));
            }
        }
        Operation::EdgeCreated | Operation::EdgeRemoved => {
            let edge = edge_payload(event)?;
            if event.issue_id.as_ref() != Some(&edge.source_id) {
                return Err(replay_conflict(
                    event,
                    "issue_id does not match edge source",
                ));
            }
        }
        Operation::MemoryCreated | Operation::MemoryUpdated | Operation::MemoryDeleted => {
            memory_snapshot(event)?;
        }
        Operation::CheckpointUpdated => {
            let value = event
                .snapshot
                .clone()
                .ok_or_else(|| replay_conflict(event, "missing checkpoint snapshot"))?;
            let checkpoint = super::checkpoints::checkpoint_snapshot(value)?;
            if event.issue_id.as_ref() != Some(&checkpoint.issue_id)
                || event.actor.as_ref() != Some(&checkpoint.actor)
            {
                return Err(replay_conflict(event, "invalid checkpoint owner"));
            }
        }
        Operation::HandoffCreated => {
            let handoff: super::Handoff = serde_json::from_value(event.payload.clone())?;
            if event.issue_id.as_ref() != Some(&handoff.issue_id)
                || event.actor.as_ref() != Some(&handoff.from_actor)
            {
                return Err(replay_conflict(event, "invalid handoff owner"));
            }
            if handoff.summary.trim().is_empty() || handoff.next_step.trim().is_empty() {
                return Err(replay_conflict(
                    event,
                    "handoff requires summary and next_step",
                ));
            }
        }
        Operation::MessageSent => {
            let message: super::Message = serde_json::from_value(
                event
                    .snapshot
                    .clone()
                    .ok_or_else(|| replay_conflict(event, "missing message snapshot"))?,
            )?;
            if message.id.trim().is_empty()
                || message.thread_id.trim().is_empty()
                || message.from_actor.trim().is_empty()
                || message.to_actor.trim().is_empty()
                || message.body.trim().is_empty()
                || event.actor.as_ref().map(Actor::as_str) != Some(message.from_actor.as_str())
            {
                return Err(replay_conflict(event, "invalid message snapshot"));
            }
        }
        Operation::MessageRead => {
            let message_id = event
                .payload
                .get("message_id")
                .and_then(Value::as_str)
                .ok_or_else(|| replay_conflict(event, "message_read is missing message_id"))?;
            let read_at = event
                .snapshot
                .as_ref()
                .and_then(|value| value.get("read_at"))
                .and_then(Value::as_str)
                .ok_or_else(|| replay_conflict(event, "message_read is missing read_at"))?;
            if message_id.trim().is_empty() || read_at.trim().is_empty() {
                return Err(replay_conflict(event, "invalid message_read payload"));
            }
        }
        Operation::IssueCompacted | Operation::IssueRestored => {
            let issue = issue_snapshot_for_replay(event)?;
            if event.issue_id.as_ref() != Some(&issue.id) {
                return Err(replay_conflict(event, "issue_id does not match snapshot"));
            }
            if event.operation == Operation::IssueCompacted && event.payload.get("level").is_none()
            {
                return Err(replay_conflict(
                    event,
                    "compaction payload is missing level",
                ));
            }
        }
    }
    Ok(())
}

fn replay_lease_payload(
    transaction: &Transaction<'_>,
    event: &JournalEvent,
) -> Result<(), StoreError> {
    if event.payload.get("lease_until").is_some() {
        let until = event
            .payload
            .get("lease_until")
            .and_then(Value::as_str)
            .ok_or_else(|| replay_conflict(event, "lease_until must be a string"))?;
        let actor = event
            .actor
            .as_ref()
            .ok_or_else(|| replay_conflict(event, "leased event requires actor"))?;
        transaction.execute(
            "UPDATE issues
             SET lease_owner = ?1, lease_started_at = ?2,
                 lease_until = ?3, heartbeat_at = ?4
             WHERE id = ?5",
            params![
                actor.as_str(),
                &event.created_at,
                until,
                &event.created_at,
                event.issue_id.as_ref().map(IssueId::as_str)
            ],
        )?;
    } else if event.payload.get("lease").is_some() {
        transaction.execute(
            "UPDATE issues
             SET lease_owner = NULL, lease_started_at = NULL,
                 lease_until = NULL, heartbeat_at = NULL
             WHERE id = ?1",
            params![event.issue_id.as_ref().map(IssueId::as_str)],
        )?;
    }
    Ok(())
}

fn replay_handoff(transaction: &Transaction<'_>, event: &JournalEvent) -> Result<(), StoreError> {
    let handoff: super::Handoff = serde_json::from_value(event.payload.clone())?;
    if !issue_exists(transaction, &handoff.issue_id)? {
        return Err(replay_conflict(
            event,
            "handoff issue predecessor is missing",
        ));
    }
    transaction.execute(
        "INSERT INTO handoffs
            (issue_id, from_actor, to_actor, summary, artifacts, next_step, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            handoff.issue_id.as_str(),
            handoff.from_actor.as_str(),
            handoff.to_actor.as_ref().map(Actor::as_str),
            &handoff.summary,
            &handoff.artifacts,
            &handoff.next_step,
            &handoff.created_at
        ],
    )?;
    Ok(())
}

pub(crate) fn issue_snapshot_for_replay(event: &JournalEvent) -> Result<Issue, StoreError> {
    let value = event
        .snapshot
        .clone()
        .ok_or_else(|| replay_conflict(event, "missing issue snapshot"))?;
    let issue: Issue = serde_json::from_value(value)?;
    IssueId::new(issue.id.as_str())?;
    issue.validate()?;
    Ok(issue)
}

fn issue_snapshot(event: &JournalEvent) -> Result<Issue, StoreError> {
    issue_snapshot_for_replay(event)
}

fn edge_payload(event: &JournalEvent) -> Result<Edge, StoreError> {
    let edge: Edge = serde_json::from_value(event.payload.clone())?;
    Ok(Edge::new(
        IssueId::new(edge.source_id.as_str())?,
        IssueId::new(edge.target_id.as_str())?,
        EdgeType::parse(edge.edge_type.as_str())?,
        edge.created_at,
    )?)
}

fn replay_issue(transaction: &Transaction<'_>, event: &JournalEvent) -> Result<(), StoreError> {
    let issue = issue_snapshot(event)?;
    let current = get_issue_from(transaction, &issue.id)?;
    match event.operation {
        Operation::IssueCreated => {
            if current.is_some() {
                return Err(replay_conflict(event, "issue already exists"));
            }
            insert_issue_snapshot(transaction, &issue)?;
        }
        Operation::IssueUpdated
        | Operation::IssueClaimed
        | Operation::IssueClosed
        | Operation::IssueCompacted
        | Operation::IssueRestored => {
            let Some(current) = current else {
                return Err(replay_conflict(event, "issue predecessor is missing"));
            };
            if current.revision.next() != issue.revision {
                return Err(replay_conflict(
                    event,
                    "issue revision predecessor is missing",
                ));
            }
            update_issue_snapshot(transaction, &issue)?;
        }
        _ => unreachable!("replay_issue only receives issue operations"),
    }
    Ok(())
}

fn replay_edge(transaction: &Transaction<'_>, event: &JournalEvent) -> Result<(), StoreError> {
    let edge = edge_payload(event)?;
    if !issue_exists(transaction, &edge.source_id)? || !issue_exists(transaction, &edge.target_id)?
    {
        return Err(replay_conflict(
            event,
            "edge endpoint predecessor is missing",
        ));
    }

    match event.operation {
        Operation::EdgeCreated => {
            if edge_exists(transaction, &edge)? {
                return Err(replay_conflict(event, "edge already exists"));
            }
            let existing = load_edges(transaction)?;
            if edge.edge_type.is_scheduling() {
                would_create_cycle(&edge.source_id, &edge.target_id, existing)?;
            }
            insert_edge_snapshot(transaction, &edge)?;
        }
        Operation::EdgeRemoved => {
            if !edge_exists(transaction, &edge)? {
                return Err(replay_conflict(event, "edge predecessor is missing"));
            }
            transaction.execute(
                "DELETE FROM edges WHERE source_id = ?1 AND target_id = ?2",
                params![edge.source_id.as_str(), edge.target_id.as_str()],
            )?;
        }
        _ => unreachable!("replay_edge only receives edge operations"),
    }
    Ok(())
}

fn memory_snapshot(event: &JournalEvent) -> Result<super::Memory, StoreError> {
    let value = event
        .snapshot
        .clone()
        .ok_or_else(|| replay_conflict(event, "missing memory snapshot"))?;
    let memory: super::Memory = serde_json::from_value(value)?;
    if memory.key.trim().is_empty()
        || memory.content.trim().is_empty()
        || memory.scope.trim().is_empty()
        || memory.source.trim().is_empty()
        || memory.revision == 0
    {
        return Err(replay_conflict(event, "invalid memory snapshot"));
    }
    Ok(memory)
}

fn replay_memory(transaction: &Transaction<'_>, event: &JournalEvent) -> Result<(), StoreError> {
    let memory = memory_snapshot(event)?;
    let key = memory.key.clone();
    let existing: Option<super::Memory> = transaction
        .query_row(
            "SELECT key, content, scope, source, created_at, updated_at, revision,
                    status, importance, confidence, expires_at, last_used_at
             FROM memories WHERE key = ?1",
            params![&key],
            |row| {
                Ok(super::Memory {
                    key: row.get(0)?,
                    content: row.get(1)?,
                    scope: row.get(2)?,
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    revision: row.get(6)?,
                    status: row.get(7)?,
                    importance: row.get(8)?,
                    confidence: row.get(9)?,
                    expires_at: row.get(10)?,
                    last_used_at: row.get(11)?,
                })
            },
        )
        .optional()?;

    match event.operation {
        Operation::MemoryCreated => {
            if existing.is_some() {
                return Err(replay_conflict(event, "memory already exists"));
            }
            insert_memory_snapshot(transaction, &memory)?;
        }
        Operation::MemoryUpdated => {
            let Some(existing) = existing else {
                return Err(replay_conflict(event, "memory predecessor is missing"));
            };
            if existing.revision + 1 != memory.revision {
                return Err(replay_conflict(
                    event,
                    "memory revision predecessor is missing",
                ));
            }
            update_memory_snapshot(transaction, &memory)?;
        }
        Operation::MemoryDeleted => {
            let Some(existing) = existing else {
                return Err(replay_conflict(event, "memory predecessor is missing"));
            };
            if existing.revision != memory.revision {
                return Err(replay_conflict(event, "memory revision does not match"));
            }
            transaction.execute("DELETE FROM memories WHERE key = ?1", params![&key])?;
        }
        _ => unreachable!("replay_memory only receives memory operations"),
    }
    Ok(())
}

fn insert_memory_snapshot(
    transaction: &Transaction<'_>,
    memory: &super::Memory,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO memories (
             key, content, scope, source, created_at, updated_at, revision,
             status, importance, confidence, expires_at, last_used_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            &memory.key,
            &memory.content,
            &memory.scope,
            &memory.source,
            &memory.created_at,
            &memory.updated_at,
            memory.revision,
            &memory.status,
            memory.importance,
            memory.confidence,
            &memory.expires_at,
            &memory.last_used_at,
        ],
    )?;
    Ok(())
}

fn update_memory_snapshot(
    transaction: &Transaction<'_>,
    memory: &super::Memory,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE memories
         SET content = ?1, scope = ?2, source = ?3, created_at = ?4,
             updated_at = ?5, revision = ?6, status = ?7, importance = ?8,
             confidence = ?9, expires_at = ?10, last_used_at = ?11
         WHERE key = ?12",
        params![
            &memory.content,
            &memory.scope,
            &memory.source,
            &memory.created_at,
            &memory.updated_at,
            memory.revision,
            &memory.status,
            memory.importance,
            memory.confidence,
            &memory.expires_at,
            &memory.last_used_at,
            &memory.key,
        ],
    )?;
    Ok(())
}

fn insert_issue_snapshot(transaction: &Transaction<'_>, issue: &Issue) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO issues (
             id, title, description, kind, status, priority, assignee,
             created_at, updated_at, defer_until, close_reason, revision
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            issue.id.as_str(),
            &issue.title,
            &issue.description,
            issue.kind.as_str(),
            issue.status.as_str(),
            issue.priority.value(),
            issue.assignee.as_ref().map(Actor::as_str),
            &issue.created_at,
            &issue.updated_at,
            &issue.defer_until,
            &issue.close_reason,
            issue.revision.value(),
        ],
    )?;
    Ok(())
}

fn update_issue_snapshot(transaction: &Transaction<'_>, issue: &Issue) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE issues
         SET title = ?1, description = ?2, kind = ?3, status = ?4, priority = ?5,
             assignee = ?6, created_at = ?7, updated_at = ?8,
             defer_until = ?9, close_reason = ?10, revision = ?11
         WHERE id = ?12",
        params![
            &issue.title,
            &issue.description,
            issue.kind.as_str(),
            issue.status.as_str(),
            issue.priority.value(),
            issue.assignee.as_ref().map(Actor::as_str),
            &issue.created_at,
            &issue.updated_at,
            &issue.defer_until,
            &issue.close_reason,
            issue.revision.value(),
            issue.id.as_str(),
        ],
    )?;
    Ok(())
}

fn insert_edge_snapshot(transaction: &Transaction<'_>, edge: &Edge) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO edges (source_id, target_id, edge_type, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            edge.source_id.as_str(),
            edge.target_id.as_str(),
            edge.edge_type.as_str(),
            &edge.created_at,
        ],
    )?;
    Ok(())
}

fn issue_exists(transaction: &Transaction<'_>, id: &IssueId) -> Result<bool, StoreError> {
    Ok(transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM issues WHERE id = ?1)",
        params![id.as_str()],
        |row| row.get(0),
    )?)
}

fn edge_exists(transaction: &Transaction<'_>, edge: &Edge) -> Result<bool, StoreError> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM edges
             WHERE source_id = ?1 AND target_id = ?2 AND edge_type = ?3
         )",
        params![
            edge.source_id.as_str(),
            edge.target_id.as_str(),
            edge.edge_type.as_str()
        ],
        |row| row.get(0),
    )?)
}

fn edge_pair_exists(transaction: &Transaction<'_>, edge: &Edge) -> Result<bool, StoreError> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM edges
             WHERE source_id = ?1 AND target_id = ?2
         )",
        params![edge.source_id.as_str(), edge.target_id.as_str()],
        |row| row.get(0),
    )?)
}

fn insert_replayed_event(
    transaction: &Transaction<'_>,
    event: &JournalEvent,
) -> Result<(), StoreError> {
    let snapshot = event
        .snapshot
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let payload = serde_json::to_string(&event.payload)?;
    transaction.execute(
        "INSERT INTO journal (
             event_id, operation, issue_id, actor, snapshot, payload, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &event.event_id,
            event.operation.as_str(),
            event.issue_id.as_ref().map(IssueId::as_str),
            event.actor.as_ref().map(Actor::as_str),
            snapshot,
            payload,
            &event.created_at,
        ],
    )?;
    Ok(())
}

fn replay_conflict(event: &JournalEvent, message: &str) -> StoreError {
    StoreError::ReplayConflict {
        message: format!("event `{}`: {message}", event.event_id),
    }
}

fn same_event(left: &JournalEvent, right: &JournalEvent) -> bool {
    left.event_id == right.event_id
        && left.operation == right.operation
        && left.issue_id == right.issue_id
        && left.actor == right.actor
        && left.created_at == right.created_at
        && left.snapshot == right.snapshot
        && left.payload == right.payload
}

pub(crate) fn append_event(
    transaction: &Transaction<'_>,
    operation: Operation,
    issue_id: Option<&IssueId>,
    actor: Option<&Actor>,
    created_at: &str,
    snapshot: Option<Value>,
    payload: Value,
) -> Result<(), StoreError> {
    let snapshot_json = snapshot.as_ref().map(serde_json::to_string).transpose()?;
    let payload_json = serde_json::to_string(&payload)?;
    let issue_id = issue_id.map(IssueId::as_str);
    let actor = actor.map(Actor::as_str);
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        operation.as_str(),
        issue_id.unwrap_or_default(),
        actor.unwrap_or_default(),
        created_at,
        snapshot_json.as_deref().unwrap_or_default(),
        payload_json
    );

    for nonce in 0..=u32::MAX {
        let event_id = format!(
            "evt-{}",
            generate_digest("journal-event", &canonical, "", "", nonce)
        );
        let exists: Option<u8> = transaction
            .query_row(
                "SELECT 1 FROM journal WHERE event_id = ?1",
                params![&event_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            continue;
        }

        transaction.execute(
            "INSERT INTO journal (
                 event_id, operation, issue_id, actor, snapshot, payload, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_id,
                operation.as_str(),
                issue_id,
                actor,
                snapshot_json,
                payload_json,
                created_at,
            ],
        )?;
        return Ok(());
    }

    Err(StoreError::EventIdCollisionExhausted)
}

pub(crate) fn snapshot<T: Serialize>(value: &T) -> Result<Value, StoreError> {
    Ok(serde_json::to_value(value)?)
}

fn journal_head(connection: &Connection) -> Result<u64, StoreError> {
    Ok(
        connection.query_row("SELECT COALESCE(MAX(seq), 0) FROM journal", [], |row| {
            row.get(0)
        })?,
    )
}

type JournalRow = (
    u64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn decode_row(row: &rusqlite::Row<'_>) -> Result<JournalRow, rusqlite::Error> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn decode_event(row: JournalRow) -> Result<JournalEvent, StoreError> {
    let (seq, event_id, operation, issue_id, actor, snapshot, payload, created_at) = row;
    let operation = Operation::parse(&operation).map_err(|error| match error {
        crate::domain::DomainError::InvalidOperation { value } => {
            StoreError::InvalidJournalOperation { value }
        }
        _ => unreachable!("operation parser only returns InvalidOperation"),
    })?;
    let issue_id = issue_id.map(IssueId::new).transpose()?;
    let actor = actor.map(Actor::new).transpose()?;
    let snapshot = snapshot
        .map(|value| serde_json::from_str(&value))
        .transpose()?;
    let payload = serde_json::from_str(&payload)?;

    Ok(JournalEvent {
        seq,
        event_id,
        operation,
        issue_id,
        actor,
        created_at,
        snapshot,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::JournalPage;
    use crate::{
        domain::{IssueDraft, IssueId, Operation, Priority},
        store::Store,
    };

    #[test]
    fn cursor_reads_only_events_after_checkpoint() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        let id = IssueId::new("ag-abcdef").expect("valid id");
        store
            .insert_draft(
                id,
                IssueDraft {
                    title: "Task".to_owned(),
                    description: String::new(),
                    priority: Priority::new(1).expect("priority"),
                    kind: crate::domain::IssueKind::Task,
                    created_at: "2026-09-26T00:00:00Z".to_owned(),
                },
            )
            .expect("insert issue");

        let page = store.list_events(0, 10).expect("read events");
        assert_eq!(page.head, 1);
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].operation, Operation::IssueCreated);

        let empty = store
            .list_events(page.events[0].seq, 10)
            .expect("read tail");
        assert_eq!(
            empty,
            JournalPage {
                events: Vec::new(),
                head: page.head,
            }
        );
    }

    #[test]
    fn recent_events_returns_the_journal_tail_in_order() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        for index in 0..3 {
            store
                .insert_draft(
                    IssueId::new(format!("ag-abcde{index}")).expect("valid id"),
                    IssueDraft {
                        title: format!("Task {index}"),
                        description: String::new(),
                        priority: Priority::new(1).expect("priority"),
                        kind: crate::domain::IssueKind::Task,
                        created_at: format!("2026-09-26T00:0{index}:00Z"),
                    },
                )
                .expect("insert issue");
        }

        let events = store.list_recent_events(2).expect("read recent events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 2);
        assert_eq!(events[1].seq, 3);
    }

    #[test]
    fn journal_append_is_transactional() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER fail_journal_insert
                 BEFORE INSERT ON journal
                 BEGIN SELECT RAISE(ABORT, 'journal disabled'); END;",
            )
            .expect("create trigger");

        let result = store.insert_draft(
            IssueId::new("ag-abcdef").expect("valid id"),
            IssueDraft {
                title: "Task".to_owned(),
                description: String::new(),
                priority: Priority::new(1).expect("priority"),
                kind: crate::domain::IssueKind::Task,
                created_at: "2026-09-26T00:00:00Z".to_owned(),
            },
        );
        assert!(result.is_err());
        assert!(store.list_issues().expect("read issues").is_empty());
        assert_eq!(store.list_events(0, 10).expect("read events").head, 0);
    }
}
