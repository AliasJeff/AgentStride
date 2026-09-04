use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::{Actor, Operation};

use super::{Store, StoreError, journal};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub key: String,
    pub content: String,
    pub scope: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub status: String,
    pub importance: u8,
    pub confidence: u8,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryGovernance {
    pub importance: u8,
    pub confidence: u8,
    pub expires_at: Option<String>,
}

impl Store {
    pub fn upsert_memory(
        &mut self,
        key: &str,
        content: &str,
        scope: &str,
        source: &str,
        now: &str,
    ) -> Result<Memory, StoreError> {
        self.upsert_memory_with_governance(
            key,
            content,
            scope,
            source,
            now,
            MemoryGovernance {
                importance: 3,
                confidence: 3,
                expires_at: None,
            },
        )
    }

    pub fn upsert_memory_with_governance(
        &mut self,
        key: &str,
        content: &str,
        scope: &str,
        source: &str,
        now: &str,
        governance: MemoryGovernance,
    ) -> Result<Memory, StoreError> {
        if key.trim().is_empty() || content.trim().is_empty() || scope.trim().is_empty() {
            return Err(StoreError::InvalidMemory {
                message: "key, content, and scope must not be empty".to_owned(),
            });
        }
        if !(1..=5).contains(&governance.importance) || !(1..=5).contains(&governance.confidence) {
            return Err(StoreError::InvalidMemory {
                message: "importance and confidence must be between 1 and 5".to_owned(),
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = get_memory_from_transaction(&transaction, key)?;
        let (created_at, revision, operation, status) = match existing {
            Some(memory) => (
                memory.created_at,
                memory.revision + 1,
                Operation::MemoryUpdated,
                memory.status,
            ),
            None => (
                now.to_owned(),
                1,
                Operation::MemoryCreated,
                "active".to_owned(),
            ),
        };
        let memory = Memory {
            key: key.to_owned(),
            content: content.to_owned(),
            scope: scope.to_owned(),
            source: source.to_owned(),
            created_at,
            updated_at: now.to_owned(),
            revision,
            status,
            importance: governance.importance,
            confidence: governance.confidence,
            expires_at: governance.expires_at,
            last_used_at: None,
        };

        transaction.execute(
            "INSERT INTO memories (
                 key, content, scope, source, created_at, updated_at, revision,
                 status, importance, confidence, expires_at, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(key) DO UPDATE SET
                 content = excluded.content,
                 scope = excluded.scope,
                 source = excluded.source,
                 updated_at = excluded.updated_at,
                 revision = excluded.revision,
                 status = excluded.status,
                 importance = excluded.importance,
                 confidence = excluded.confidence,
                 expires_at = excluded.expires_at",
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
        let actor = Actor::new(source)?;
        journal::append_event(
            &transaction,
            operation,
            None,
            Some(&actor),
            now,
            Some(journal::snapshot(&memory)?),
            serde_json::json!({ "key": memory.key }),
        )?;
        transaction.commit()?;
        Ok(memory)
    }

    pub fn get_memory(&self, key: &str) -> Result<Option<Memory>, StoreError> {
        get_memory(&self.connection, key)
    }

    pub fn list_memories(
        &self,
        search: Option<&str>,
        scope: Option<&str>,
        include_archived: bool,
        now: Option<&str>,
    ) -> Result<Vec<Memory>, StoreError> {
        let search = search.map(|value| format!("%{}%", value.to_lowercase()));
        let mut statement = self.connection.prepare(
            "SELECT key, content, scope, source, created_at, updated_at, revision,
                    status, importance, confidence, expires_at, last_used_at
             FROM memories
             WHERE (?1 IS NULL OR lower(key) LIKE ?1 OR lower(content) LIKE ?1)
               AND (?2 IS NULL OR scope = ?2)
               AND (?3 = 1 OR status = 'active')
               AND (?3 = 1 OR ?4 IS NULL OR expires_at IS NULL OR expires_at > ?4)
             ORDER BY importance DESC, confidence DESC, updated_at DESC, key",
        )?;
        let rows =
            statement.query_map(params![search, scope, include_archived, now], decode_memory)?;
        rows.map(|row| row.map_err(StoreError::from)).collect()
    }

    pub fn delete_memory(&mut self, key: &str, now: &str) -> Result<Option<Memory>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(memory) = get_memory_from_transaction(&transaction, key)? else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute("DELETE FROM memories WHERE key = ?1", params![key])?;
        let actor = Actor::new(memory.source.clone())?;
        journal::append_event(
            &transaction,
            Operation::MemoryDeleted,
            None,
            Some(&actor),
            now,
            Some(journal::snapshot(&memory)?),
            serde_json::json!({ "key": memory.key }),
        )?;
        transaction.commit()?;
        Ok(Some(memory))
    }

    pub fn archive_stale_memories(
        &mut self,
        now: &str,
        dry_run: bool,
    ) -> Result<Vec<Memory>, StoreError> {
        let stale = self
            .list_memories(None, None, false, None)?
            .into_iter()
            .filter(|memory| {
                memory.status == "active"
                    && memory
                        .expires_at
                        .as_deref()
                        .is_some_and(|expires_at| expires_at <= now)
            })
            .collect::<Vec<_>>();
        if dry_run || stale.is_empty() {
            return Ok(stale);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut archived = Vec::with_capacity(stale.len());
        for memory in &stale {
            let mut snapshot = memory.clone();
            snapshot.status = "archived".to_owned();
            snapshot.updated_at = now.to_owned();
            snapshot.revision += 1;
            transaction.execute(
                "UPDATE memories
                 SET status = 'archived', updated_at = ?1, revision = revision + 1
                 WHERE key = ?2 AND status = 'active'",
                params![now, &memory.key],
            )?;
            let actor = Actor::new(memory.source.clone())?;
            journal::append_event(
                &transaction,
                Operation::MemoryUpdated,
                None,
                Some(&actor),
                now,
                Some(journal::snapshot(&snapshot)?),
                serde_json::json!({ "key": memory.key, "governance": "archive_stale" }),
            )?;
            archived.push(snapshot);
        }
        transaction.commit()?;
        Ok(archived)
    }

    pub fn mark_memories_used(&mut self, keys: &[String], now: &str) -> Result<(), StoreError> {
        if keys.is_empty() {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for key in keys {
            transaction.execute(
                "UPDATE memories SET last_used_at = ?1 WHERE key = ?2 AND status = 'active'",
                params![now, key],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn get_memory(connection: &Connection, key: &str) -> Result<Option<Memory>, StoreError> {
    connection
        .query_row(
            "SELECT key, content, scope, source, created_at, updated_at, revision,
                    status, importance, confidence, expires_at, last_used_at
             FROM memories WHERE key = ?1",
            params![key],
            decode_memory,
        )
        .optional()
        .map_err(StoreError::from)
}

fn get_memory_from_transaction(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<Option<Memory>, StoreError> {
    transaction
        .query_row(
            "SELECT key, content, scope, source, created_at, updated_at, revision,
                    status, importance, confidence, expires_at, last_used_at
             FROM memories WHERE key = ?1",
            params![key],
            decode_memory,
        )
        .optional()
        .map_err(StoreError::from)
}

fn decode_memory(row: &rusqlite::Row<'_>) -> Result<Memory, rusqlite::Error> {
    Ok(Memory {
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
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn memory_round_trips_and_replaces_atomically() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");

        let created = store
            .upsert_memory(
                "storage-rule",
                "writes use transactions",
                "workspace",
                "agent-a",
                "2026-09-26T00:00:00Z",
            )
            .expect("create memory");
        assert_eq!(created.revision, 1);

        let updated = store
            .upsert_memory(
                "storage-rule",
                "writes and journal append share a transaction",
                "workspace",
                "agent-a",
                "2026-09-26T00:01:00Z",
            )
            .expect("update memory");
        assert_eq!(updated.revision, 2);
        assert_eq!(
            store
                .get_memory("storage-rule")
                .expect("read memory")
                .expect("memory exists")
                .content,
            updated.content
        );
    }

    #[test]
    fn memory_search_and_delete_are_scoped() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        store
            .upsert_memory(
                "rust-rule",
                "use Rust",
                "workspace",
                "agent-a",
                "2026-09-26T00:00:00Z",
            )
            .expect("create workspace memory");
        store
            .upsert_memory(
                "agent-rule",
                "use tests",
                "agent",
                "agent-a",
                "2026-09-26T00:01:00Z",
            )
            .expect("create agent memory");

        let memories = store
            .list_memories(Some("RUST"), Some("workspace"), false, None)
            .expect("search memories");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].key, "rust-rule");

        let deleted = store
            .delete_memory("rust-rule", "2026-09-26T00:02:00Z")
            .expect("delete memory")
            .expect("memory was deleted");
        assert_eq!(deleted.key, "rust-rule");
        assert!(
            store
                .get_memory("rust-rule")
                .expect("read deleted memory")
                .is_none()
        );
        assert!(
            store
                .get_memory("agent-rule")
                .expect("read remaining memory")
                .is_some()
        );
    }
}
