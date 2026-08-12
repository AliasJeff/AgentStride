use serde::{Deserialize, Serialize};

use super::{DomainError, IssueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Blocks,
    ParentChild,
    Related,
}

impl EdgeType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::ParentChild => "parent_child",
            Self::Related => "related",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "blocks" => Ok(Self::Blocks),
            "parent_child" => Ok(Self::ParentChild),
            "related" => Ok(Self::Related),
            value => Err(DomainError::InvalidEdgeType {
                value: value.to_owned(),
            }),
        }
    }

    pub const fn is_scheduling(self) -> bool {
        matches!(self, Self::Blocks | Self::ParentChild)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EdgeKey {
    pub source_id: IssueId,
    pub target_id: IssueId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub source_id: IssueId,
    pub target_id: IssueId,
    pub edge_type: EdgeType,
    pub created_at: String,
}

impl Edge {
    pub fn new(
        source_id: IssueId,
        target_id: IssueId,
        edge_type: EdgeType,
        created_at: String,
    ) -> Result<Self, DomainError> {
        if source_id == target_id {
            return Err(DomainError::SelfDependency {
                issue_id: source_id.to_string(),
            });
        }
        Ok(Self {
            source_id,
            target_id,
            edge_type,
            created_at,
        })
    }

    pub fn natural_key(&self) -> EdgeKey {
        EdgeKey {
            source_id: self.source_id.clone(),
            target_id: self.target_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Edge, EdgeType};
    use crate::domain::{DomainError, IssueId};

    #[test]
    fn scheduling_edges_are_classified_explicitly() {
        assert!(EdgeType::Blocks.is_scheduling());
        assert!(EdgeType::ParentChild.is_scheduling());
        assert!(!EdgeType::Related.is_scheduling());
    }

    #[test]
    fn self_edge_is_rejected() {
        let issue_id = IssueId::new("ag-abcdef").expect("valid id");
        assert_eq!(
            Edge::new(
                issue_id.clone(),
                issue_id,
                EdgeType::Blocks,
                "2026-09-26T00:00:00Z".to_owned()
            )
            .expect_err("self-edge must fail"),
            DomainError::SelfDependency {
                issue_id: "ag-abcdef".to_owned()
            }
        );
    }
}
