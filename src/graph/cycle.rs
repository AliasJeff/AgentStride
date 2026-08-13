use std::collections::HashSet;

use crate::domain::{DomainError, Edge, IssueId};

pub fn would_create_cycle(
    source: &IssueId,
    target: &IssueId,
    edges: impl IntoIterator<Item = Edge>,
) -> Result<(), DomainError> {
    if source == target {
        return Err(DomainError::SelfDependency {
            issue_id: source.to_string(),
        });
    }

    let mut adjacency: Vec<(IssueId, IssueId)> = edges
        .into_iter()
        .filter(|edge| edge.edge_type.is_scheduling())
        .map(|edge| (edge.source_id, edge.target_id))
        .collect();
    adjacency.push((source.clone(), target.clone()));

    let mut visited = HashSet::new();
    let mut stack = vec![target.clone()];
    while let Some(node) = stack.pop() {
        if !visited.insert(node.clone()) {
            continue;
        }
        if node == *source {
            return Err(DomainError::DependencyCycle);
        }
        for (edge_source, edge_target) in &adjacency {
            if edge_source == &node {
                stack.push(edge_target.clone());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::would_create_cycle;
    use crate::domain::{Edge, EdgeType, IssueId};

    fn id(value: &str) -> IssueId {
        IssueId::new(value).expect("valid issue id")
    }

    fn edge(source: &str, target: &str, edge_type: EdgeType) -> Edge {
        Edge::new(
            id(source),
            id(target),
            edge_type,
            "2026-09-26T00:00:00Z".to_owned(),
        )
        .expect("valid edge")
    }

    #[test]
    fn rejects_direct_cycle() {
        let edges = [edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks)];
        assert!(would_create_cycle(&id("ag-aaaaaa"), &id("ag-bbbbbb"), edges).is_err());
    }

    #[test]
    fn rejects_cycle_through_parent_child() {
        let edges = [
            edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::ParentChild),
            edge("ag-cccccc", "ag-bbbbbb", EdgeType::Blocks),
        ];
        assert!(would_create_cycle(&id("ag-aaaaaa"), &id("ag-cccccc"), edges).is_err());
    }

    #[test]
    fn ignores_related_edges() {
        let edges = [edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Related)];
        assert!(would_create_cycle(&id("ag-aaaaaa"), &id("ag-bbbbbb"), edges).is_ok());
    }

    #[test]
    fn terminates_on_diamond_graph() {
        let edges = [
            edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks),
            edge("ag-cccccc", "ag-aaaaaa", EdgeType::Blocks),
            edge("ag-dddddd", "ag-bbbbbb", EdgeType::Blocks),
            edge("ag-dddddd", "ag-cccccc", EdgeType::Blocks),
        ];
        assert!(would_create_cycle(&id("ag-aaaaaa"), &id("ag-dddddd"), edges).is_err());
    }
}
