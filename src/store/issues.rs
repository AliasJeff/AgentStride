use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::domain::{
    Actor, Issue, IssueDraft, IssueId, IssueKind, IssuePatch, Priority, Revision, Status,
};

use super::{Store, StoreError, journal};

#[derive(Debug, Clone, Serialize)]
pub struct EpicSummary {
    pub epic: Issue,
    pub children: Vec<Issue>,
    pub completed: usize,
    pub total: usize,
    pub complete: bool,
}

impl Store {
    pub fn epic_summary(&self, id: &IssueId) -> Result<EpicSummary, StoreError> {
        let epic = self
            .get_issue(id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        if epic.kind != IssueKind::Epic {
            return Err(StoreError::InvalidEpic {
                message: format!("issue `{id}` is not an epic"),
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT i.id, i.title, i.description, i.kind, i.status, i.priority,
                    i.assignee, i.created_at, i.updated_at, i.defer_until,
                    i.close_reason, i.revision
             FROM edges e
             JOIN issues i ON i.id = e.source_id
             WHERE e.target_id = ?1 AND e.edge_type = 'parent_child'
             ORDER BY i.priority, i.created_at, i.id",
        )?;
        let rows = statement.query_map(params![id.as_str()], |row| {
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
        })?;
        let children = rows
            .map(|row| row.map_err(StoreError::from).and_then(decode_issue))
            .collect::<Result<Vec<_>, _>>()?;
        let total = children.len();
        let completed = children
            .iter()
            .filter(|child| child.status == Status::Closed)
            .count();
        Ok(EpicSummary {
            epic,
            children,
            completed,
            total,
            complete: total > 0 && completed == total,
        })
    }

    pub fn insert_issue(&mut self, issue: &Issue) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
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
        journal::append_event(
            &transaction,
            crate::domain::Operation::IssueCreated,
            Some(&issue.id),
            None,
            &issue.created_at,
            Some(journal::snapshot(issue)?),
            serde_json::json!({}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_issue(&self, id: &IssueId) -> Result<Option<Issue>, StoreError> {
        get_issue_from(&self.connection, id)
    }

    pub fn list_issues(&self) -> Result<Vec<Issue>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, description, kind, status, priority, assignee,
                    created_at, updated_at, defer_until, close_reason, revision
             FROM issues
             ORDER BY priority, created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
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
        })?;

        rows.map(|row| row.map_err(StoreError::from).and_then(decode_issue))
            .collect()
    }

    pub fn insert_draft(&mut self, id: IssueId, draft: IssueDraft) -> Result<Issue, StoreError> {
        let issue = Issue::from_draft(id, draft)?;
        self.insert_issue(&issue)?;
        Ok(issue)
    }

    pub fn update_issue(
        &mut self,
        id: &IssueId,
        patch: IssuePatch,
        updated_at: String,
        expected_revision: Option<Revision>,
    ) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        verify_revision(&issue, expected_revision)?;
        let previous_revision = issue.revision;
        issue.apply_patch(patch, updated_at)?;
        let changed = transaction.execute(
            "UPDATE issues
             SET title = ?1, description = ?2, priority = ?3,
                 updated_at = ?4, revision = ?5
             WHERE id = ?6 AND revision = ?7",
            params![
                &issue.title,
                &issue.description,
                issue.priority.value(),
                &issue.updated_at,
                issue.revision.value(),
                issue.id.as_str(),
                previous_revision.value(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: previous_revision.value(),
            });
        }
        journal::append_event(
            &transaction,
            crate::domain::Operation::IssueUpdated,
            Some(&issue.id),
            None,
            &issue.updated_at,
            Some(journal::snapshot(&issue)?),
            serde_json::json!({}),
        )?;
        transaction.commit()?;
        Ok(issue)
    }

    pub fn close_issue(
        &mut self,
        id: &IssueId,
        reason: String,
        updated_at: String,
        expected_revision: Option<Revision>,
    ) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        verify_revision(&issue, expected_revision)?;
        let previous_revision = issue.revision;
        issue.close(reason, updated_at)?;
        let changed = transaction.execute(
            "UPDATE issues
             SET status = ?1, updated_at = ?2, close_reason = ?3, revision = ?4,
                 lease_owner = NULL, lease_started_at = NULL,
                 lease_until = NULL, heartbeat_at = NULL
             WHERE id = ?5 AND revision = ?6",
            params![
                issue.status.as_str(),
                &issue.updated_at,
                &issue.close_reason,
                issue.revision.value(),
                issue.id.as_str(),
                previous_revision.value(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: previous_revision.value(),
            });
        }
        journal::append_event(
            &transaction,
            crate::domain::Operation::IssueClosed,
            Some(&issue.id),
            None,
            &issue.updated_at,
            Some(journal::snapshot(&issue)?),
            serde_json::json!({ "reason": issue.close_reason, "lease": "released" }),
        )?;
        transaction.commit()?;
        Ok(issue)
    }
}

pub(crate) fn get_issue_from(
    connection: &Connection,
    id: &IssueId,
) -> Result<Option<Issue>, StoreError> {
    let row = connection
        .query_row(
            "SELECT id, title, description, kind, status, priority, assignee,
                    created_at, updated_at, defer_until, close_reason, revision
             FROM issues
             WHERE id = ?1",
            params![id.as_str()],
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

fn verify_revision(issue: &Issue, expected_revision: Option<Revision>) -> Result<(), StoreError> {
    if let Some(expected) = expected_revision
        && expected != issue.revision
    {
        return Err(StoreError::RevisionMismatch {
            expected: expected.value(),
        });
    }
    Ok(())
}

pub(crate) type IssueRow = (
    String,
    String,
    String,
    String,
    String,
    u8,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    u64,
);

pub(crate) fn decode_issue(row: IssueRow) -> Result<Issue, StoreError> {
    let (
        id,
        title,
        description,
        kind,
        status,
        priority,
        assignee,
        created_at,
        updated_at,
        defer_until,
        close_reason,
        revision,
    ) = row;
    Ok(Issue {
        id: IssueId::new(id)?,
        title,
        description,
        kind: IssueKind::parse(&kind)?,
        status: Status::parse(&status)?,
        priority: Priority::new(priority)?,
        assignee: assignee.map(Actor::new).transpose()?,
        created_at,
        updated_at,
        defer_until,
        close_reason,
        revision: Revision::new(revision),
    })
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::domain::IssueKind;
    use crate::{
        domain::{IssueDraft, IssueId, IssuePatch, Priority, Revision},
        store::StoreError,
    };

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        store
    }

    fn draft() -> IssueDraft {
        IssueDraft {
            title: "Design storage".to_owned(),
            description: "Choose the schema".to_owned(),
            priority: Priority::new(1).expect("valid priority"),
            kind: IssueKind::Task,
            created_at: "2026-09-26T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn issue_round_trips_through_sqlite() {
        let mut store = store();
        let id = IssueId::new("ag-abcdef").expect("valid id");
        let expected = store
            .insert_draft(id.clone(), draft())
            .expect("insert issue");

        let actual = store
            .get_issue(&id)
            .expect("read issue")
            .expect("issue exists");

        assert_eq!(actual, expected);
    }

    #[test]
    fn duplicate_issue_id_is_rejected() {
        let mut store = store();
        let id = IssueId::new("ag-abcdef").expect("valid id");
        store
            .insert_draft(id.clone(), draft())
            .expect("insert first issue");

        assert!(store.insert_draft(id, draft()).is_err());
    }

    #[test]
    fn list_orders_by_priority_then_creation_time() {
        let mut store = store();
        store
            .insert_draft(
                IssueId::new("ag-aaaaaa").expect("valid id"),
                IssueDraft {
                    priority: Priority::new(2).expect("valid priority"),
                    created_at: "2026-09-26T00:00:00Z".to_owned(),
                    ..draft()
                },
            )
            .expect("insert later issue");
        store
            .insert_draft(IssueId::new("ag-bbbbbb").expect("valid id"), draft())
            .expect("insert earlier issue");

        let issues = store.list_issues().expect("list issues");
        assert_eq!(issues[0].id.as_str(), "ag-bbbbbb");
        assert_eq!(issues[1].id.as_str(), "ag-aaaaaa");
    }

    #[test]
    fn stale_revision_does_not_update_issue() {
        let mut store = store();
        let id = IssueId::new("ag-abcdef").expect("valid id");
        store
            .insert_draft(id.clone(), draft())
            .expect("insert issue");
        store
            .update_issue(
                &id,
                IssuePatch {
                    title: Some("Updated".to_owned()),
                    ..IssuePatch::default()
                },
                "2026-09-26T00:01:00Z".to_owned(),
                None,
            )
            .expect("first update");

        let result = store.update_issue(
            &id,
            IssuePatch {
                title: Some("Stale".to_owned()),
                ..IssuePatch::default()
            },
            "2026-09-26T00:02:00Z".to_owned(),
            Some(Revision::initial()),
        );
        assert!(matches!(
            result,
            Err(StoreError::RevisionMismatch { expected: 1 })
        ));
        assert_eq!(
            store
                .get_issue(&id)
                .expect("read issue")
                .expect("issue exists")
                .title,
            "Updated"
        );
    }
}
