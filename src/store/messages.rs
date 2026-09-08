use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::{Actor, Operation, generate_digest};

use super::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub parent_id: Option<String>,
    pub from_actor: String,
    pub to_actor: String,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub created_at: String,
    pub read_at: Option<String>,
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn send_message(
        &mut self,
        from_actor: &str,
        to_actor: &str,
        subject: &str,
        body: &str,
        thread_id: Option<&str>,
        parent_id: Option<&str>,
        kind: &str,
        created_at: &str,
    ) -> Result<Message, StoreError> {
        if from_actor.trim().is_empty()
            || to_actor.trim().is_empty()
            || body.trim().is_empty()
            || created_at.trim().is_empty()
        {
            return Err(StoreError::InvalidMessage {
                message: "from, to, body, and created_at are required".to_owned(),
            });
        }
        let thread_id = thread_id.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "thr-{}",
                &generate_digest(
                    "message-thread",
                    from_actor,
                    &format!("{to_actor}\n{subject}\n{body}"),
                    created_at,
                    0,
                )[..16]
            )
        });
        let id = format!(
            "msg-{}",
            &generate_digest(
                "message",
                &thread_id,
                &format!("{from_actor}\n{to_actor}\n{body}"),
                created_at,
                0,
            )[..16]
        );
        let message = Message {
            id,
            thread_id,
            parent_id: parent_id.map(str::to_owned),
            from_actor: from_actor.to_owned(),
            to_actor: to_actor.to_owned(),
            kind: if kind.trim().is_empty() {
                "message".to_owned()
            } else {
                kind.to_owned()
            },
            subject: subject.to_owned(),
            body: body.to_owned(),
            created_at: created_at.to_owned(),
            read_at: None,
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO messages
             (id, thread_id, parent_id, from_actor, to_actor, kind, subject, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &message.id,
                &message.thread_id,
                &message.parent_id,
                &message.from_actor,
                &message.to_actor,
                &message.kind,
                &message.subject,
                &message.body,
                &message.created_at
            ],
        )?;
        let actor = Actor::new(from_actor.to_owned())?;
        super::journal::append_event(
            &transaction,
            Operation::MessageSent,
            None,
            Some(&actor),
            created_at,
            Some(serde_json::to_value(&message)?),
            serde_json::json!({
                "message_id": message.id,
                "thread_id": message.thread_id,
                "to_actor": message.to_actor,
                "kind": message.kind
            }),
        )?;
        transaction.commit()?;
        Ok(message)
    }

    pub fn list_inbox(
        &self,
        actor: &str,
        thread_id: Option<&str>,
    ) -> Result<Vec<Message>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, thread_id, parent_id, from_actor, to_actor, kind,
                    subject, body, created_at, read_at
             FROM messages
             WHERE to_actor = ?1 AND (?2 IS NULL OR thread_id = ?2)
             ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![actor, thread_id], decode_message)?;
        rows.map(|row| row.map_err(StoreError::from)).collect()
    }

    pub fn list_thread(&self, thread_id: &str) -> Result<Vec<Message>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, thread_id, parent_id, from_actor, to_actor, kind,
                    subject, body, created_at, read_at
             FROM messages WHERE thread_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![thread_id], decode_message)?;
        rows.map(|row| row.map_err(StoreError::from)).collect()
    }

    pub fn mark_message_read(&mut self, id: &str, read_at: &str) -> Result<bool, StoreError> {
        if read_at.trim().is_empty() {
            return Err(StoreError::InvalidMessage {
                message: "read_at is required".to_owned(),
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let message: Option<Message> = transaction
            .query_row(
                "SELECT id, thread_id, parent_id, from_actor, to_actor, kind,
                        subject, body, created_at, read_at
                 FROM messages WHERE id = ?1",
                params![id],
                decode_message,
            )
            .optional()?;
        let Some(message) = message else {
            transaction.commit()?;
            return Ok(false);
        };
        if message.read_at.is_some() {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE messages SET read_at = ?1 WHERE id = ?2 AND read_at IS NULL",
            params![read_at, id],
        )?;
        super::journal::append_event(
            &transaction,
            Operation::MessageRead,
            None,
            None,
            read_at,
            Some(serde_json::json!({
                "message_id": id,
                "read_at": read_at,
            })),
            serde_json::json!({ "message_id": id }),
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn replay_message(
        transaction: &rusqlite::Transaction<'_>,
        message: &Message,
    ) -> Result<(), StoreError> {
        let exists: Option<String> = transaction
            .query_row(
                "SELECT id FROM messages WHERE id = ?1",
                params![&message.id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(StoreError::ReplayConflict {
                message: format!("message `{}` already exists", message.id),
            });
        }
        transaction.execute(
            "INSERT INTO messages
             (id, thread_id, parent_id, from_actor, to_actor, kind, subject, body, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &message.id,
                &message.thread_id,
                &message.parent_id,
                &message.from_actor,
                &message.to_actor,
                &message.kind,
                &message.subject,
                &message.body,
                &message.created_at,
                &message.read_at
            ],
        )?;
        Ok(())
    }

    pub(crate) fn replay_message_read(
        transaction: &rusqlite::Transaction<'_>,
        event: &crate::store::JournalEvent,
    ) -> Result<(), StoreError> {
        let message_id = event
            .payload
            .get("message_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StoreError::ReplayConflict {
                message: "message_read is missing message_id".to_owned(),
            })?;
        let read_at = event
            .snapshot
            .as_ref()
            .and_then(|value| value.get("read_at"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StoreError::ReplayConflict {
                message: "message_read is missing read_at".to_owned(),
            })?;
        let updated = transaction.execute(
            "UPDATE messages SET read_at = ?1
             WHERE id = ?2 AND read_at IS NULL",
            params![read_at, message_id],
        )?;
        if updated == 0 {
            let exists: Option<String> = transaction
                .query_row(
                    "SELECT id FROM messages WHERE id = ?1",
                    params![message_id],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(StoreError::ReplayConflict {
                    message: format!("message `{message_id}` predecessor is missing"),
                });
            }
        }
        Ok(())
    }
}

fn decode_message(row: &rusqlite::Row<'_>) -> Result<Message, rusqlite::Error> {
    Ok(Message {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        parent_id: row.get(2)?,
        from_actor: row.get(3)?,
        to_actor: row.get(4)?,
        kind: row.get(5)?,
        subject: row.get(6)?,
        body: row.get(7)?,
        created_at: row.get(8)?,
        read_at: row.get(9)?,
    })
}
