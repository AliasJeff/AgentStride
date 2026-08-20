use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::{
    domain::{Actor, Issue, IssueId, Revision},
    store::ready::query_ready,
};

use super::{Store, StoreError, issues::decode_issue, journal};

impl Store {
    pub fn claim_issue(
        &mut self,
        id: &IssueId,
        actor: Actor,
        now: &str,
        expected_revision: Option<Revision>,
    ) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        if let Some(assignee) = &issue.assignee
            && assignee != &actor
        {
            return Err(crate::domain::DomainError::AlreadyClaimed {
                assignee: assignee.as_str().to_owned(),
            }
            .into());
        }
        if let Some(expected) = expected_revision
            && expected != issue.revision
        {
            return Err(StoreError::RevisionMismatch {
                expected: expected.value(),
            });
        }
        if issue.status == crate::domain::Status::InProgress
            && issue.assignee.as_ref() == Some(&actor)
        {
            transaction.commit()?;
            return Ok(issue);
        }
        if !query_ready(&transaction, now)?
            .iter()
            .any(|candidate| candidate.id == *id)
        {
            return Err(StoreError::IssueNotReady { id: id.to_string() });
        }

        let mut claimed = issue;
        claimed.claim(actor, now.to_owned())?;
        write_claim(&transaction, &claimed)?;
        journal::append_event(
            &transaction,
            crate::domain::Operation::IssueClaimed,
            Some(&claimed.id),
            claimed.assignee.as_ref(),
            &claimed.updated_at,
            Some(journal::snapshot(&claimed)?),
            serde_json::json!({}),
        )?;
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn claim_next(&mut self, actor: Actor, now: &str) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = query_ready(&transaction, now)?
            .into_iter()
            .next()
            .ok_or(crate::domain::DomainError::NoReadyWork)?;
        let mut claimed = candidate;
        claimed.claim(actor, now.to_owned())?;
        write_claim(&transaction, &claimed)?;
        journal::append_event(
            &transaction,
            crate::domain::Operation::IssueClaimed,
            Some(&claimed.id),
            claimed.assignee.as_ref(),
            &claimed.updated_at,
            Some(journal::snapshot(&claimed)?),
            serde_json::json!({}),
        )?;
        transaction.commit()?;
        Ok(claimed)
    }
}

fn write_claim(transaction: &rusqlite::Transaction<'_>, issue: &Issue) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE issues
         SET status = ?1, assignee = ?2, updated_at = ?3, revision = ?4
         WHERE id = ?5 AND status = 'open' AND revision = ?6",
        rusqlite::params![
            issue.status.as_str(),
            issue.assignee.as_ref().map(Actor::as_str),
            &issue.updated_at,
            issue.revision.value(),
            issue.id.as_str(),
            issue.revision.value().saturating_sub(1),
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::RevisionMismatch {
            expected: issue.revision.value().saturating_sub(1),
        });
    }
    Ok(())
}

fn get_issue(
    connection: &rusqlite::Transaction<'_>,
    id: &IssueId,
) -> Result<Option<Issue>, StoreError> {
    let row = connection
        .query_row(
            "SELECT id, title, description, kind, status, priority, assignee,
                    created_at, updated_at, defer_until, close_reason, revision
             FROM issues
             WHERE id = ?1",
            rusqlite::params![id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u8>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, u64>(11)?,
                ))
            },
        )
        .optional()?;
    row.map(decode_issue).transpose()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::Store;
    use crate::domain::{Actor, IssueDraft, IssueId, Priority, Status};

    fn id(value: &str) -> IssueId {
        IssueId::new(value).expect("valid issue id")
    }

    fn draft(title: &str, priority: u8) -> IssueDraft {
        IssueDraft {
            title: title.to_owned(),
            description: String::new(),
            priority: Priority::new(priority).expect("valid priority"),
            kind: crate::domain::IssueKind::Task,
            created_at: "2026-09-26T00:00:00Z".to_owned(),
        }
    }

    fn memory_store() -> Store {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        store
            .insert_draft(id("ag-aaaaaa"), draft("first", 1))
            .expect("insert issue");
        store
    }

    #[test]
    fn claim_sets_owner_and_is_idempotent_for_same_actor() {
        let mut store = memory_store();
        let actor = Actor::new("worker-a").expect("valid actor");

        let first = store
            .claim_issue(
                &id("ag-aaaaaa"),
                actor.clone(),
                "2026-09-26T00:01:00Z",
                None,
            )
            .expect("first claim");
        let second = store
            .claim_issue(&id("ag-aaaaaa"), actor, "2026-09-26T00:02:00Z", None)
            .expect("same actor retry");

        assert_eq!(first.status, Status::InProgress);
        assert_eq!(first.assignee, second.assignee);
        assert_eq!(first.revision, second.revision);
        assert_eq!(first.updated_at, second.updated_at);
    }

    #[test]
    fn foreign_actor_cannot_claim_an_owned_issue() {
        let mut store = memory_store();
        store
            .claim_issue(
                &id("ag-aaaaaa"),
                Actor::new("worker-a").expect("valid actor"),
                "2026-09-26T00:01:00Z",
                None,
            )
            .expect("first claim");

        let result = store.claim_issue(
            &id("ag-aaaaaa"),
            Actor::new("worker-b").expect("valid actor"),
            "2026-09-26T00:02:00Z",
            None,
        );
        assert!(matches!(
            result,
            Err(crate::store::StoreError::Domain(
                crate::domain::DomainError::AlreadyClaimed { .. }
            ))
        ));
    }

    #[test]
    fn competing_connections_only_claim_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("claims.db");
        let mut setup = Store::open(&path).expect("open database");
        setup.migrate().expect("migrate database");
        setup
            .insert_draft(id("ag-aaaaaa"), draft("first", 1))
            .expect("insert issue");
        drop(setup);

        let barrier = Arc::new(Barrier::new(2));
        thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let first_path = path.clone();
            let first = scope.spawn(move || {
                let mut store = Store::open(&first_path).expect("open first worker");
                first_barrier.wait();
                store.claim_next(
                    Actor::new("worker-a").expect("valid actor"),
                    "2026-09-26T00:01:00Z",
                )
            });

            let second_barrier = Arc::clone(&barrier);
            let second_path = path.clone();
            let second = scope.spawn(move || {
                let mut store = Store::open(&second_path).expect("open second worker");
                second_barrier.wait();
                store.claim_next(
                    Actor::new("worker-b").expect("valid actor"),
                    "2026-09-26T00:01:00Z",
                )
            });

            let results = [
                first.join().expect("first worker"),
                second.join().expect("second worker"),
            ];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        });
    }
}
