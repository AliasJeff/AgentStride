use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("title must not be empty")]
    InvalidTitle,

    #[error("title exceeds the maximum length of {max} bytes")]
    TitleTooLong { max: usize },

    #[error("priority must be between 0 and 4")]
    InvalidPriority,

    #[error("actor must not be empty")]
    InvalidActor,

    #[error("timestamp must not be empty")]
    InvalidTimestamp,

    #[error("invalid issue id `{value}`")]
    InvalidIssueId { value: String },

    #[error("invalid edge type `{value}`")]
    InvalidEdgeType { value: String },

    #[error("invalid status `{value}`")]
    InvalidStatus { value: String },

    #[error("invalid issue kind `{value}`")]
    InvalidIssueKind { value: String },

    #[error("cannot transition from `{from}` to `{to}`")]
    InvalidTransition { from: String, to: String },

    #[error("an issue cannot depend on itself: `{issue_id}`")]
    SelfDependency { issue_id: String },

    #[error("dependency cycle would be created")]
    DependencyCycle,

    #[error("issue is already claimed by `{assignee}`")]
    AlreadyClaimed { assignee: String },

    #[error("revision mismatch: expected {expected}, actual {actual}")]
    RevisionMismatch { expected: u64, actual: u64 },

    #[error("no ready work is available")]
    NoReadyWork,

    #[error("invalid operation `{value}`")]
    InvalidOperation { value: String },

    #[error("revision must be greater than zero")]
    InvalidRevision,
}
