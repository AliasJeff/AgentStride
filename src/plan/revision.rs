use serde::Serialize;

use super::{GitRepository, PlanError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanRevision {
    pub commit: String,
}

impl PlanRevision {
    pub fn current(repository: &GitRepository) -> Result<Self, PlanError> {
        Ok(Self {
            commit: repository.current_revision()?,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.commit
    }
}

impl std::fmt::Display for PlanRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.commit)
    }
}
