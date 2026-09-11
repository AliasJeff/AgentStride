mod export;
mod git;
mod proposal;
mod reconcile;
mod revision;

pub use export::{PlanSnapshot, export_snapshot};
pub use git::{GitFile, GitRepository, GitStatus};
pub use proposal::{PlanProposal, ProposalStatus};
pub use reconcile::{PlanChange, PlanGraph, reconcile};
pub use revision::PlanRevision;

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("plan path `{path}` is not a directory")]
    InvalidRoot { path: PathBuf },

    #[error("failed to access plan file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to serialize plan: {0}")]
    Json(#[from] serde_json::Error),

    #[error("failed to read plan state: {0}")]
    Storage(#[from] crate::store::StoreError),

    #[error("git command failed: {command}: {message}")]
    Git { command: String, message: String },

    #[error("git repository has no commit yet")]
    NoCommit,

    #[error("invalid git revision `{revision}`")]
    InvalidRevision { revision: String },

    #[error("invalid plan graph: {message}")]
    InvalidGraph { message: String },
}
