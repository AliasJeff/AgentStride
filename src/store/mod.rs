mod checkpoints;
mod claims;
mod compaction;
mod coordination;
mod edges;
mod issues;
mod journal;
mod memories;
mod messages;
mod migrations;
mod plan;
mod ready;
mod runtime;
mod sqlite;

#[cfg(test)]
mod coordination_tests;

pub use crate::plan::PlanProposal;
pub use checkpoints::Checkpoint;
pub use compaction::{CompactionCandidate, CompactionMetadata};
pub use coordination::{Handoff, Lease};
pub use issues::EpicSummary;
pub use journal::{JournalEvent, JournalPage, SnapshotImportResult};
pub use memories::{Memory, MemoryGovernance};
pub use messages::Message;
pub use runtime::{
    RuntimeEventPage, RuntimeRecoveryReport, RuntimeRunSummary, RuntimeStateResult,
    RuntimeTaskLease, RuntimeVerification,
};
pub use sqlite::Store;

use thiserror::Error;

use crate::domain::DomainError;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("issue `{id}` was not found")]
    IssueNotFound { id: String },

    #[error("issue `{id}` is not ready for claiming")]
    IssueNotReady { id: String },

    #[error("revision mismatch: expected {expected}")]
    RevisionMismatch { expected: u64 },

    #[error("dependency already exists: `{source_id}` -> `{target_id}`")]
    DuplicateDependency {
        source_id: String,
        target_id: String,
    },

    #[error("migration {version} is invalid: {reason}")]
    InvalidMigration { version: u32, reason: String },

    #[error("invalid journal operation `{value}`")]
    InvalidJournalOperation { value: String },

    #[error("journal event id space is exhausted")]
    EventIdCollisionExhausted,

    #[error("replay conflict: {message}")]
    ReplayConflict { message: String },

    #[error("invalid memory: {message}")]
    InvalidMemory { message: String },

    #[error("invalid epic: {message}")]
    InvalidEpic { message: String },

    #[error("invalid checkpoint: {message}")]
    InvalidCheckpoint { message: String },

    #[error("checkpoint for issue `{id}` is not owned by the current actor")]
    CheckpointNotOwned { id: String },

    #[error("invalid lease: {message}")]
    InvalidLease { message: String },

    #[error("issue `{id}` has an active lease owned by `{owner}`")]
    LeaseOwned { id: String, owner: String },

    #[error("issue `{id}` has no lease owned by `{actor}`")]
    LeaseNotOwned { id: String, actor: String },

    #[error("handoff is invalid: {message}")]
    InvalidHandoff { message: String },

    #[error("compaction is invalid: {message}")]
    InvalidCompaction { message: String },

    #[error("invalid message: {message}")]
    InvalidMessage { message: String },

    #[error("issue `{id}` has no restorable snapshot")]
    NoSnapshot { id: String },

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),

    #[error(transparent)]
    RuntimeToken(#[from] crate::runtime::token::RuntimeTokenError),

    #[error("runtime run `{run_id}` already exists")]
    RuntimeRunExists { run_id: String },

    #[error("runtime run `{run_id}` was not found")]
    RuntimeRunNotFound { run_id: String },

    #[error("runtime run `{run_id}` changed concurrently")]
    RuntimeConflict { run_id: String },

    #[error("no durable runtime task is available")]
    NoRuntimeTask,

    #[error("runtime task `{task_id}` changed concurrently")]
    RuntimeTaskConflict { task_id: String },

    #[error("runtime timer `{timer_id}` changed concurrently")]
    RuntimeTimerConflict { timer_id: String },

    #[error("invalid plan proposal: {message}")]
    InvalidPlanProposal { message: String },

    #[error("plan proposal `{proposal_id}` was not found")]
    PlanProposalNotFound { proposal_id: String },

    #[error("plan proposal `{proposal_id}` cannot be changed: {message}")]
    PlanProposalConflict {
        proposal_id: String,
        message: String,
    },

    #[error(
        "plan conflict for run `{run_id}`: proposal expects `{expected}`, current is `{actual}`"
    )]
    PlanConflict {
        run_id: String,
        expected: String,
        actual: String,
    },

    #[error("plan revision `{revision}` is not approved for run `{run_id}`")]
    PlanRevisionNotApproved { run_id: String, revision: String },
}
