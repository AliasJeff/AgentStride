use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::plan::{PlanProposal, ProposalStatus};

use super::{Store, StoreError};

impl Store {
    pub fn create_plan_proposal(
        &mut self,
        proposal: &PlanProposal,
    ) -> Result<PlanProposal, StoreError> {
        validate_proposal(proposal)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run_exists: Option<String> = transaction
            .query_row(
                "SELECT run_id FROM runtime_runs WHERE run_id = ?1",
                params![&proposal.run_id],
                |row| row.get(0),
            )
            .optional()?;
        if run_exists.is_none() {
            return Err(StoreError::RuntimeRunNotFound {
                run_id: proposal.run_id.clone(),
            });
        }
        transaction.execute(
            "INSERT INTO plan_proposals (
                 proposal_id, run_id, base_revision, head_revision,
                 author, reason, status, created_at, approved_by, approved_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', ?7, NULL, NULL)",
            params![
                &proposal.proposal_id,
                &proposal.run_id,
                &proposal.base_revision,
                &proposal.head_revision,
                &proposal.author,
                &proposal.reason,
                &proposal.created_at,
            ],
        )?;
        transaction.commit()?;
        Ok(proposal.clone())
    }

    pub fn get_plan_proposal(&self, proposal_id: &str) -> Result<Option<PlanProposal>, StoreError> {
        self.connection
            .query_row(
                "SELECT proposal_id, run_id, base_revision, head_revision,
                        author, reason, status, created_at, approved_by, approved_at
                 FROM plan_proposals
                 WHERE proposal_id = ?1",
                params![proposal_id],
                decode_proposal,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn approve_plan_proposal(
        &mut self,
        proposal_id: &str,
        approver: &str,
        approved_at: &str,
    ) -> Result<PlanProposal, StoreError> {
        if approver.trim().is_empty() || approved_at.trim().is_empty() {
            return Err(StoreError::InvalidPlanProposal {
                message: "approver and approval timestamp are required".to_owned(),
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let proposal = transaction
            .query_row(
                "SELECT proposal_id, run_id, base_revision, head_revision,
                        author, reason, status, created_at, approved_by, approved_at
                 FROM plan_proposals
                 WHERE proposal_id = ?1",
                params![proposal_id],
                decode_proposal,
            )
            .optional()?
            .ok_or_else(|| StoreError::PlanProposalNotFound {
                proposal_id: proposal_id.to_owned(),
            })?;
        if proposal.status != ProposalStatus::Proposed {
            return Err(StoreError::PlanProposalConflict {
                proposal_id: proposal_id.to_owned(),
                message: format!("proposal is already {}", proposal.status.as_str()),
            });
        }
        let current_revision: String = transaction.query_row(
            "SELECT plan_revision FROM runtime_runs WHERE run_id = ?1",
            params![&proposal.run_id],
            |row| row.get(0),
        )?;
        if current_revision != proposal.base_revision {
            return Err(StoreError::PlanConflict {
                run_id: proposal.run_id.clone(),
                expected: proposal.base_revision,
                actual: current_revision,
            });
        }
        transaction.execute(
            "UPDATE plan_proposals
             SET status = 'approved', approved_by = ?1, approved_at = ?2
             WHERE proposal_id = ?3 AND status = 'proposed'",
            params![approver, approved_at, proposal_id],
        )?;
        transaction.commit()?;
        self.get_plan_proposal(proposal_id)?
            .ok_or_else(|| StoreError::PlanProposalNotFound {
                proposal_id: proposal_id.to_owned(),
            })
    }

    pub fn ensure_approved_plan_revision(
        &self,
        run_id: &str,
        revision: &str,
    ) -> Result<(), StoreError> {
        let approved: Option<String> = self
            .connection
            .query_row(
                "SELECT proposal_id
                 FROM plan_proposals
                 WHERE run_id = ?1 AND head_revision = ?2 AND status = 'approved'
                 ORDER BY approved_at DESC, proposal_id
                 LIMIT 1",
                params![run_id, revision],
                |row| row.get(0),
            )
            .optional()?;
        if approved.is_none() {
            return Err(StoreError::PlanRevisionNotApproved {
                run_id: run_id.to_owned(),
                revision: revision.to_owned(),
            });
        }
        Ok(())
    }
}

pub(crate) fn ensure_approved_plan_revision_in_transaction(
    transaction: &Transaction<'_>,
    run_id: &str,
    base_revision: &str,
    revision: &str,
) -> Result<(), StoreError> {
    let proposal: Option<(String, String)> = transaction
        .query_row(
            "SELECT base_revision, proposal_id
             FROM plan_proposals
             WHERE run_id = ?1 AND head_revision = ?2 AND status = 'approved'
             ORDER BY approved_at DESC, proposal_id
             LIMIT 1",
            params![run_id, revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((proposal_base, _proposal_id)) = proposal else {
        return Err(StoreError::PlanRevisionNotApproved {
            run_id: run_id.to_owned(),
            revision: revision.to_owned(),
        });
    };
    if proposal_base != base_revision {
        return Err(StoreError::PlanConflict {
            run_id: run_id.to_owned(),
            expected: proposal_base,
            actual: base_revision.to_owned(),
        });
    }
    Ok(())
}

fn validate_proposal(proposal: &PlanProposal) -> Result<(), StoreError> {
    for (name, value) in [
        ("proposal_id", &proposal.proposal_id),
        ("run_id", &proposal.run_id),
        ("base_revision", &proposal.base_revision),
        ("head_revision", &proposal.head_revision),
        ("author", &proposal.author),
        ("reason", &proposal.reason),
        ("created_at", &proposal.created_at),
    ] {
        if value.trim().is_empty() {
            return Err(StoreError::InvalidPlanProposal {
                message: format!("{name} must not be empty"),
            });
        }
    }
    if proposal.status != ProposalStatus::Proposed
        || proposal.approved_by.is_some()
        || proposal.approved_at.is_some()
    {
        return Err(StoreError::InvalidPlanProposal {
            message: "new proposals must be unapproved".to_owned(),
        });
    }
    Ok(())
}

fn decode_proposal(row: &rusqlite::Row<'_>) -> Result<PlanProposal, rusqlite::Error> {
    let status: String = row.get(6)?;
    Ok(PlanProposal {
        proposal_id: row.get(0)?,
        run_id: row.get(1)?,
        base_revision: row.get(2)?,
        head_revision: row.get(3)?,
        author: row.get(4)?,
        reason: row.get(5)?,
        status: ProposalStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(6, "status".to_owned(), rusqlite::types::Type::Text)
        })?,
        created_at: row.get(7)?,
        approved_by: row.get(8)?,
        approved_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::{
        plan::{PlanProposal, ProposalStatus},
        runtime::{Command, CommandKind, RunId},
    };

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        store
            .start_run(
                RunId::new("proposal-run").expect("run"),
                "plan-1",
                Command::new(
                    "start-proposal-run",
                    CommandKind::StartRun {
                        now: "2026-09-29T00:00:00Z".to_owned(),
                    },
                )
                .expect("command"),
            )
            .expect("start");
        store
    }

    fn proposal() -> PlanProposal {
        PlanProposal {
            proposal_id: "proposal-1".to_owned(),
            run_id: "proposal-run".to_owned(),
            base_revision: "plan-1".to_owned(),
            head_revision: "plan-2".to_owned(),
            author: "agent-a".to_owned(),
            reason: "add research task".to_owned(),
            status: ProposalStatus::Proposed,
            created_at: "2026-09-29T00:00:01Z".to_owned(),
            approved_by: None,
            approved_at: None,
        }
    }

    #[test]
    fn proposal_requires_approval_before_revision_is_usable() {
        let mut store = store();
        store.create_plan_proposal(&proposal()).expect("create");
        assert!(matches!(
            store.ensure_approved_plan_revision("proposal-run", "plan-2"),
            Err(super::super::StoreError::PlanRevisionNotApproved { .. })
        ));

        let approved = store
            .approve_plan_proposal("proposal-1", "reviewer", "2026-09-29T00:00:02Z")
            .expect("approve");
        assert_eq!(approved.status, ProposalStatus::Approved);
        assert_eq!(approved.approved_by.as_deref(), Some("reviewer"));
        store
            .ensure_approved_plan_revision("proposal-run", "plan-2")
            .expect("approved revision");
    }

    #[test]
    fn stale_proposal_cannot_be_approved() {
        let mut store = store();
        store.create_plan_proposal(&proposal()).expect("create");
        store
            .connection
            .execute(
                "UPDATE runtime_runs SET plan_revision = 'plan-2' WHERE run_id = 'proposal-run'",
                [],
            )
            .expect("advance run");

        assert!(matches!(
            store.approve_plan_proposal("proposal-1", "reviewer", "2026-09-29T00:00:02Z"),
            Err(super::super::StoreError::PlanConflict { .. })
        ));
    }
}
