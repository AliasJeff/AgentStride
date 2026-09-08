use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::domain::{Actor, Issue, IssueId, Operation, Status};

use super::{Store, StoreError, issues::get_issue_from, ready::query_ready};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: Actor,
    pub started_at: String,
    pub until: String,
    pub heartbeat_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub issue_id: IssueId,
    pub from_actor: Actor,
    pub to_actor: Option<Actor>,
    pub summary: String,
    pub artifacts: String,
    pub next_step: String,
    pub created_at: String,
}

type LeaseRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl Store {
    pub fn claim_next_with_lease(
        &mut self,
        actor: Actor,
        now: &str,
        until: &str,
    ) -> Result<Issue, StoreError> {
        validate_lease_times(now, until)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = query_ready(&transaction, now)?
            .into_iter()
            .next()
            .ok_or(crate::domain::DomainError::NoReadyWork)?;
        let mut claimed = candidate;
        claimed.claim(actor.clone(), now.to_owned())?;
        let changed = transaction.execute(
            "UPDATE issues
             SET status = ?1, assignee = ?2, updated_at = ?3, revision = ?4,
                 lease_owner = ?5, lease_started_at = ?6,
                 lease_until = ?7, heartbeat_at = ?8
             WHERE id = ?9 AND status = 'open' AND revision = ?10",
            params![
                claimed.status.as_str(),
                claimed.assignee.as_ref().map(Actor::as_str),
                &claimed.updated_at,
                claimed.revision.value(),
                actor.as_str(),
                now,
                until,
                now,
                claimed.id.as_str(),
                claimed.revision.value().saturating_sub(1),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: claimed.revision.value().saturating_sub(1),
            });
        }
        super::journal::append_event(
            &transaction,
            Operation::IssueClaimed,
            Some(&claimed.id),
            Some(&actor),
            now,
            Some(super::journal::snapshot(&claimed)?),
            serde_json::json!({ "lease_until": until }),
        )?;
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn claim_issue_with_lease(
        &mut self,
        id: &IssueId,
        actor: Actor,
        now: &str,
        until: &str,
    ) -> Result<Issue, StoreError> {
        validate_lease_times(now, until)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        if let Some(current) = lease_for(&transaction, id)?
            && current.until.as_str() > now
            && current.owner != actor
        {
            return Err(StoreError::LeaseOwned {
                id: id.to_string(),
                owner: current.owner.as_str().to_owned(),
            });
        }
        if issue.status == Status::Open {
            if !query_ready(&transaction, now)?
                .iter()
                .any(|candidate| candidate.id == *id)
            {
                return Err(StoreError::IssueNotReady { id: id.to_string() });
            }
        } else if issue.assignee.as_ref() != Some(&actor) {
            return Err(StoreError::IssueNotReady { id: id.to_string() });
        } else {
            let changed = transaction.execute(
                "UPDATE issues
                 SET lease_owner = ?1, lease_started_at = ?2,
                     lease_until = ?3, heartbeat_at = ?4
                 WHERE id = ?5 AND assignee = ?6",
                params![actor.as_str(), now, until, now, id.as_str(), actor.as_str()],
            )?;
            if changed != 1 {
                return Err(StoreError::RevisionMismatch {
                    expected: issue.revision.value(),
                });
            }
            transaction.commit()?;
            return Ok(issue);
        }

        let mut claimed = issue;
        if claimed.status == Status::Open {
            claimed.claim(actor.clone(), now.to_owned())?;
        }
        let changed = transaction.execute(
            "UPDATE issues
             SET status = ?1, assignee = ?2, updated_at = ?3, revision = ?4,
                 lease_owner = ?5, lease_started_at = ?6,
                 lease_until = ?7, heartbeat_at = ?8
             WHERE id = ?9 AND revision = ?10",
            params![
                claimed.status.as_str(),
                claimed.assignee.as_ref().map(Actor::as_str),
                &claimed.updated_at,
                claimed.revision.value(),
                actor.as_str(),
                now,
                until,
                now,
                claimed.id.as_str(),
                claimed.revision.value().saturating_sub(1),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: claimed.revision.value().saturating_sub(1),
            });
        }
        super::journal::append_event(
            &transaction,
            Operation::IssueClaimed,
            Some(&claimed.id),
            Some(&actor),
            now,
            Some(super::journal::snapshot(&claimed)?),
            serde_json::json!({ "lease_until": until }),
        )?;
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn heartbeat(
        &mut self,
        id: &IssueId,
        actor: &Actor,
        now: &str,
        until: &str,
    ) -> Result<Lease, StoreError> {
        validate_lease_times(now, until)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = lease_for(&transaction, id)?.ok_or_else(|| StoreError::LeaseNotOwned {
            id: id.to_string(),
            actor: actor.as_str().to_owned(),
        })?;
        if current.owner != *actor {
            return Err(StoreError::LeaseNotOwned {
                id: id.to_string(),
                actor: actor.as_str().to_owned(),
            });
        }
        let changed = transaction.execute(
            "UPDATE issues
             SET lease_until = ?1, heartbeat_at = ?2
             WHERE id = ?3 AND lease_owner = ?4 AND lease_until = ?5",
            params![until, now, id.as_str(), actor.as_str(), &current.until],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch { expected: 0 });
        }
        let lease = Lease {
            owner: actor.clone(),
            started_at: current.started_at,
            until: until.to_owned(),
            heartbeat_at: now.to_owned(),
        };
        transaction.commit()?;
        Ok(lease)
    }

    pub fn release_lease(
        &mut self,
        id: &IssueId,
        actor: &Actor,
        now: &str,
    ) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        let current = lease_for(&transaction, id)?.ok_or_else(|| StoreError::LeaseNotOwned {
            id: id.to_string(),
            actor: actor.as_str().to_owned(),
        })?;
        ensure_lease_owner(id, &current, actor)?;
        let next_revision = issue.revision.next();
        let changed = transaction.execute(
            "UPDATE issues
             SET lease_owner = NULL, lease_started_at = NULL,
                 lease_until = NULL, heartbeat_at = NULL,
                 assignee = NULL, status = 'open',
                 updated_at = ?1, revision = ?2
             WHERE id = ?3 AND revision = ?4 AND lease_owner = ?5",
            params![
                now,
                next_revision.value(),
                id.as_str(),
                issue.revision.value(),
                actor.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: issue.revision.value(),
            });
        }
        let released = reset_issue(issue, now);
        super::journal::append_event(
            &transaction,
            Operation::IssueUpdated,
            Some(id),
            Some(actor),
            now,
            Some(super::journal::snapshot(&released)?),
            serde_json::json!({ "lease": "released" }),
        )?;
        transaction.commit()?;
        Ok(released)
    }

    pub fn reclaim_lease(
        &mut self,
        id: &IssueId,
        supervisor: &Actor,
        now: &str,
    ) -> Result<Issue, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let issue = get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        let current = lease_for(&transaction, id)?.ok_or_else(|| StoreError::InvalidLease {
            message: "issue has no lease".to_owned(),
        })?;
        if current.until.as_str() > now {
            return Err(StoreError::InvalidLease {
                message: "lease has not expired".to_owned(),
            });
        }
        let next_revision = issue.revision.next();
        let changed = transaction.execute(
            "UPDATE issues
             SET lease_owner = NULL, lease_started_at = NULL,
                 lease_until = NULL, heartbeat_at = NULL,
                 assignee = NULL, status = 'open',
                 updated_at = ?1, revision = ?2
             WHERE id = ?3 AND revision = ?4 AND lease_until = ?5",
            params![
                now,
                next_revision.value(),
                id.as_str(),
                issue.revision.value(),
                &current.until
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionMismatch {
                expected: issue.revision.value(),
            });
        }
        let reclaimed = reset_issue(issue, now);
        super::journal::append_event(
            &transaction,
            Operation::IssueUpdated,
            Some(id),
            Some(supervisor),
            now,
            Some(super::journal::snapshot(&reclaimed)?),
            serde_json::json!({
                "lease": "reclaimed",
                "previous_owner": current.owner.as_str()
            }),
        )?;
        transaction.commit()?;
        Ok(reclaimed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_handoff(
        &mut self,
        id: &IssueId,
        from_actor: &Actor,
        to_actor: Option<Actor>,
        summary: &str,
        artifacts: &str,
        next_step: &str,
        created_at: &str,
    ) -> Result<Handoff, StoreError> {
        validate_handoff(summary, next_step, created_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        get_issue_from(&transaction, id)?
            .ok_or_else(|| StoreError::IssueNotFound { id: id.to_string() })?;
        let current = lease_for(&transaction, id)?.ok_or_else(|| StoreError::LeaseNotOwned {
            id: id.to_string(),
            actor: from_actor.as_str().to_owned(),
        })?;
        ensure_lease_owner(id, &current, from_actor)?;
        let handoff = Handoff {
            issue_id: id.clone(),
            from_actor: from_actor.clone(),
            to_actor,
            summary: summary.to_owned(),
            artifacts: artifacts.to_owned(),
            next_step: next_step.to_owned(),
            created_at: created_at.to_owned(),
        };
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
        super::journal::append_event(
            &transaction,
            Operation::HandoffCreated,
            Some(id),
            Some(from_actor),
            created_at,
            None,
            serde_json::to_value(&handoff)?,
        )?;
        transaction.commit()?;
        Ok(handoff)
    }

    pub fn get_lease(&self, id: &IssueId) -> Result<Option<Lease>, StoreError> {
        lease_for(&self.connection, id)
    }

    pub fn list_handoffs(&self, id: &IssueId) -> Result<Vec<Handoff>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT from_actor, to_actor, summary, artifacts, next_step, created_at
             FROM handoffs WHERE issue_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (from, to, summary, artifacts, next_step, created_at) =
                row.map_err(StoreError::from)?;
            Ok(Handoff {
                issue_id: id.clone(),
                from_actor: Actor::new(from)?,
                to_actor: to.map(Actor::new).transpose()?,
                summary,
                artifacts,
                next_step,
                created_at,
            })
        })
        .collect()
    }
}

fn reset_issue(mut issue: Issue, now: &str) -> Issue {
    issue.status = Status::Open;
    issue.assignee = None;
    issue.updated_at = now.to_owned();
    issue.revision = issue.revision.next();
    issue
}

fn ensure_lease_owner(id: &IssueId, lease: &Lease, actor: &Actor) -> Result<(), StoreError> {
    if lease.owner != *actor {
        return Err(StoreError::LeaseNotOwned {
            id: id.to_string(),
            actor: actor.as_str().to_owned(),
        });
    }
    Ok(())
}

fn validate_lease_times(now: &str, until: &str) -> Result<(), StoreError> {
    if now.trim().is_empty() || until.trim().is_empty() || until <= now {
        return Err(StoreError::InvalidLease {
            message: "lease timestamps must be non-empty and until must be after now".to_owned(),
        });
    }
    Ok(())
}

fn validate_handoff(summary: &str, next_step: &str, created_at: &str) -> Result<(), StoreError> {
    if summary.trim().is_empty() || next_step.trim().is_empty() || created_at.trim().is_empty() {
        return Err(StoreError::InvalidHandoff {
            message: "summary, next_step, and created_at are required".to_owned(),
        });
    }
    Ok(())
}

fn lease_for(connection: &Connection, id: &IssueId) -> Result<Option<Lease>, StoreError> {
    let row: Option<LeaseRow> = connection
        .query_row(
            "SELECT lease_owner, lease_started_at, lease_until, heartbeat_at
                 FROM issues WHERE id = ?1",
            params![id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((owner, started_at, until, heartbeat_at)) = row else {
        return Ok(None);
    };
    let Some(owner) = owner else {
        return Ok(None);
    };
    Ok(Some(Lease {
        owner: Actor::new(owner)?,
        started_at: started_at.unwrap_or_default(),
        until: until.unwrap_or_default(),
        heartbeat_at: heartbeat_at.unwrap_or_default(),
    }))
}
