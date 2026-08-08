use serde::{Deserialize, Serialize};

use super::{DomainError, IssueId};

const MAX_TITLE_BYTES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    Task,
    Epic,
}

impl IssueKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Epic => "epic",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "task" => Ok(Self::Task),
            "epic" => Ok(Self::Epic),
            value => Err(DomainError::InvalidIssueKind {
                value: value.to_owned(),
            }),
        }
    }
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Closed => "closed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "open" => Ok(Self::Open),
            "in_progress" => Ok(Self::InProgress),
            "closed" => Ok(Self::Closed),
            value => Err(DomainError::InvalidStatus {
                value: value.to_owned(),
            }),
        }
    }

    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Open, Self::Open)
                | (Self::Open, Self::InProgress)
                | (Self::Open, Self::Closed)
                | (Self::InProgress, Self::InProgress)
                | (Self::InProgress, Self::Closed)
                | (Self::Closed, Self::Closed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Priority(u8);

impl Priority {
    pub fn new(value: u8) -> Result<Self, DomainError> {
        if value <= 4 {
            Ok(Self(value))
        } else {
            Err(DomainError::InvalidPriority)
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Actor(String);

impl Actor {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(DomainError::InvalidActor)
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDraft {
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub kind: IssueKind,
    pub created_at: String,
}

impl IssueDraft {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_title(&self.title)?;
        if self.created_at.trim().is_empty() {
            return Err(DomainError::InvalidTimestamp);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    pub title: String,
    pub description: String,
    pub kind: IssueKind,
    pub status: Status,
    pub priority: Priority,
    pub assignee: Option<Actor>,
    pub created_at: String,
    pub updated_at: String,
    pub defer_until: Option<String>,
    pub close_reason: String,
    pub revision: Revision,
}

impl Issue {
    pub fn from_draft(id: IssueId, draft: IssueDraft) -> Result<Self, DomainError> {
        draft.validate()?;
        Ok(Self {
            id,
            title: draft.title,
            description: draft.description,
            kind: draft.kind,
            status: Status::Open,
            priority: draft.priority,
            assignee: None,
            created_at: draft.created_at.clone(),
            updated_at: draft.created_at,
            defer_until: None,
            close_reason: String::new(),
            revision: Revision::initial(),
        })
    }

    pub fn apply_patch(
        &mut self,
        patch: IssuePatch,
        updated_at: String,
    ) -> Result<(), DomainError> {
        if let Some(title) = patch.title {
            validate_title(&title)?;
            self.title = title;
        }
        if let Some(description) = patch.description {
            self.description = description;
        }
        if let Some(priority) = patch.priority {
            self.priority = priority;
        }
        self.updated_at = updated_at;
        self.revision = self.revision.next();
        Ok(())
    }

    pub fn transition_to(&mut self, next: Status, updated_at: String) -> Result<(), DomainError> {
        if !self.status.can_transition_to(&next) {
            return Err(DomainError::InvalidTransition {
                from: self.status.as_str().to_owned(),
                to: next.as_str().to_owned(),
            });
        }
        self.status = next;
        self.updated_at = updated_at;
        self.revision = self.revision.next();
        Ok(())
    }

    pub fn close(&mut self, reason: String, updated_at: String) -> Result<(), DomainError> {
        self.transition_to(Status::Closed, updated_at)?;
        self.close_reason = reason;
        Ok(())
    }

    pub fn claim(&mut self, actor: Actor, updated_at: String) -> Result<(), DomainError> {
        if let Some(assignee) = &self.assignee {
            if assignee != &actor {
                return Err(DomainError::AlreadyClaimed {
                    assignee: assignee.as_str().to_owned(),
                });
            }
            if self.status == Status::InProgress {
                return Ok(());
            }
        }
        self.transition_to(Status::InProgress, updated_at)?;
        self.assignee = Some(actor);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_title(&self.title)?;
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err(DomainError::InvalidTimestamp);
        }
        if self.revision.value() == 0 {
            return Err(DomainError::InvalidRevision);
        }
        if let Some(actor) = &self.assignee {
            Actor::new(actor.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    IssueCreated,
    IssueUpdated,
    IssueClaimed,
    IssueClosed,
    EdgeCreated,
    EdgeRemoved,
    MemoryCreated,
    MemoryUpdated,
    MemoryDeleted,
    CheckpointUpdated,
    HandoffCreated,
    MessageSent,
    MessageRead,
    IssueCompacted,
    IssueRestored,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IssueCreated => "issue_created",
            Self::IssueUpdated => "issue_updated",
            Self::IssueClaimed => "issue_claimed",
            Self::IssueClosed => "issue_closed",
            Self::EdgeCreated => "edge_created",
            Self::EdgeRemoved => "edge_removed",
            Self::MemoryCreated => "memory_created",
            Self::MemoryUpdated => "memory_updated",
            Self::MemoryDeleted => "memory_deleted",
            Self::CheckpointUpdated => "checkpoint_updated",
            Self::HandoffCreated => "handoff_created",
            Self::MessageSent => "message_sent",
            Self::MessageRead => "message_read",
            Self::IssueCompacted => "issue_compacted",
            Self::IssueRestored => "issue_restored",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "issue_created" => Ok(Self::IssueCreated),
            "issue_updated" => Ok(Self::IssueUpdated),
            "issue_claimed" => Ok(Self::IssueClaimed),
            "issue_closed" => Ok(Self::IssueClosed),
            "edge_created" => Ok(Self::EdgeCreated),
            "edge_removed" => Ok(Self::EdgeRemoved),
            "memory_created" => Ok(Self::MemoryCreated),
            "memory_updated" => Ok(Self::MemoryUpdated),
            "memory_deleted" => Ok(Self::MemoryDeleted),
            "checkpoint_updated" => Ok(Self::CheckpointUpdated),
            "handoff_created" => Ok(Self::HandoffCreated),
            "message_sent" => Ok(Self::MessageSent),
            "message_read" => Ok(Self::MessageRead),
            "issue_compacted" => Ok(Self::IssueCompacted),
            "issue_restored" => Ok(Self::IssueRestored),
            value => Err(DomainError::InvalidOperation {
                value: value.to_owned(),
            }),
        }
    }
}

fn validate_title(title: &str) -> Result<(), DomainError> {
    if title.trim().is_empty() {
        return Err(DomainError::InvalidTitle);
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(DomainError::TitleTooLong {
            max: MAX_TITLE_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Actor, Issue, IssueDraft, IssueKind, IssuePatch, Operation, Priority, Status};
    use crate::domain::{DomainError, IssueId};

    fn issue() -> Issue {
        Issue::from_draft(
            IssueId::new("ag-abcdef").expect("valid id"),
            IssueDraft {
                title: "Design storage".to_owned(),
                description: "Choose the schema".to_owned(),
                priority: Priority::new(1).expect("valid priority"),
                kind: IssueKind::Task,
                created_at: "2026-09-26T00:00:00Z".to_owned(),
            },
        )
        .expect("valid issue")
    }

    #[test]
    fn lifecycle_transitions_are_monotonic() {
        let mut issue = issue();
        issue
            .transition_to(Status::InProgress, "2026-09-26T00:01:00Z".to_owned())
            .expect("open to in progress");
        issue
            .transition_to(Status::Closed, "2026-09-26T00:02:00Z".to_owned())
            .expect("in progress to closed");
        assert_eq!(issue.status, Status::Closed);
    }

    #[test]
    fn closed_issue_cannot_reopen() {
        let mut issue = issue();
        issue
            .transition_to(Status::Closed, "2026-09-26T00:01:00Z".to_owned())
            .expect("open to closed");
        assert_eq!(
            issue
                .transition_to(Status::Open, "2026-09-26T00:02:00Z".to_owned())
                .expect_err("closed to open must fail"),
            DomainError::InvalidTransition {
                from: "closed".to_owned(),
                to: "open".to_owned()
            }
        );
    }

    #[test]
    fn patch_validates_before_changing_title() {
        let mut issue = issue();
        let original = issue.clone();
        let result = issue.apply_patch(
            IssuePatch {
                title: Some("  ".to_owned()),
                ..IssuePatch::default()
            },
            "2026-09-26T00:01:00Z".to_owned(),
        );

        assert_eq!(result, Err(DomainError::InvalidTitle));
        assert_eq!(issue, original);
    }

    #[test]
    fn actor_rejects_blank_values() {
        assert_eq!(Actor::new("  "), Err(DomainError::InvalidActor));
    }

    #[test]
    fn enums_use_stable_wire_names() {
        assert_eq!(
            serde_json::to_string(&Status::InProgress).expect("serialize status"),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&Operation::IssueCreated).expect("serialize operation"),
            "\"issue_created\""
        );
    }
}
