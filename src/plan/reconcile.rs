use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{
    domain::{Edge, Issue, IssueId, IssueKind},
    graph::would_create_cycle,
    runtime::{MutableState, TaskId, TaskStatus, TaskSupersession},
};

use super::{GitRepository, PlanError};

const ISSUES_PATH: &str = ".tasks/issues.jsonl";
const EDGES_PATH: &str = ".tasks/edges.jsonl";

#[derive(Debug, Clone, PartialEq)]
pub struct PlanGraph {
    pub issues: BTreeMap<IssueId, Issue>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanChange {
    pub discovered: Vec<TaskId>,
    pub ready: Vec<TaskId>,
    pub invalidated: Vec<TaskId>,
    pub superseded: Vec<TaskSupersession>,
}

#[derive(Debug, Deserialize)]
struct IssueRecord {
    record_kind: String,
    #[serde(flatten)]
    issue: Issue,
}

#[derive(Debug, Deserialize)]
struct EdgeRecord {
    record_kind: String,
    #[serde(flatten)]
    edge: Edge,
}

impl PlanGraph {
    pub fn from_git(repository: &GitRepository, revision: &str) -> Result<Self, PlanError> {
        let issues =
            parse_issue_records(&repository.read_file_at_revision(revision, ISSUES_PATH)?)?;
        let edges = parse_edge_records(&repository.read_file_at_revision(revision, EDGES_PATH)?)?;
        Self::new(issues, edges)
    }

    pub fn new(issues: Vec<Issue>, edges: Vec<Edge>) -> Result<Self, PlanError> {
        let mut issue_map = BTreeMap::new();
        for issue in issues {
            if issue_map.insert(issue.id.clone(), issue).is_some() {
                return Err(PlanError::InvalidGraph {
                    message: "duplicate issue id".to_owned(),
                });
            }
        }

        let mut seen_edges = BTreeSet::new();
        let mut accepted_edges = Vec::with_capacity(edges.len());
        for edge in edges {
            if !issue_map.contains_key(&edge.source_id) || !issue_map.contains_key(&edge.target_id)
            {
                return Err(PlanError::InvalidGraph {
                    message: format!(
                        "edge {} -> {} references an unknown issue",
                        edge.source_id, edge.target_id
                    ),
                });
            }
            if !seen_edges.insert(edge.natural_key()) {
                return Err(PlanError::InvalidGraph {
                    message: format!("duplicate edge {} -> {}", edge.source_id, edge.target_id),
                });
            }
            if edge.edge_type.is_scheduling() {
                would_create_cycle(
                    &edge.source_id,
                    &edge.target_id,
                    accepted_edges.iter().cloned(),
                )
                .map_err(|error| PlanError::InvalidGraph {
                    message: error.to_string(),
                })?;
            }
            accepted_edges.push(edge);
        }

        Ok(Self {
            issues: issue_map,
            edges: accepted_edges,
        })
    }

    pub fn ready_frontier(
        &self,
        state: &MutableState,
        candidates: impl IntoIterator<Item = IssueId>,
    ) -> Vec<TaskId> {
        let mut ready = candidates
            .into_iter()
            .filter(|issue_id| self.is_ready(issue_id, state))
            .map(|issue_id| TaskId::new(issue_id.to_string()).expect("issue ids are non-empty"))
            .collect::<Vec<_>>();
        ready.sort();
        ready
    }

    fn is_ready(&self, issue_id: &IssueId, state: &MutableState) -> bool {
        let mut stack = vec![issue_id.clone()];
        let mut visited = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            for edge in self
                .edges
                .iter()
                .filter(|edge| edge.source_id == current && edge.edge_type.is_scheduling())
            {
                let blocker_id = match TaskId::new(edge.target_id.to_string()) {
                    Ok(id) => id,
                    Err(_) => return false,
                };
                let Some(blocker) = state.tasks.get(&blocker_id) else {
                    return false;
                };
                if blocker.status != TaskStatus::Succeeded {
                    return false;
                }
                stack.push(edge.target_id.clone());
            }
        }
        true
    }
}

pub fn reconcile(
    old: &PlanGraph,
    new: &PlanGraph,
    state: &MutableState,
    superseded: Vec<TaskSupersession>,
    _reason: &str,
) -> Result<PlanChange, PlanError> {
    let old_ids = old
        .issues
        .values()
        .filter(|issue| issue.kind == IssueKind::Task)
        .map(|issue| &issue.id)
        .collect::<BTreeSet<_>>();
    let new_ids = new
        .issues
        .values()
        .filter(|issue| issue.kind == IssueKind::Task)
        .map(|issue| &issue.id)
        .collect::<BTreeSet<_>>();
    let superseded_old = superseded
        .iter()
        .map(|change| IssueId::new(change.task_id.to_string()).expect("task ids are non-empty"))
        .collect::<BTreeSet<_>>();
    for change in &superseded {
        let old_id =
            IssueId::new(change.task_id.to_string()).map_err(|error| PlanError::InvalidGraph {
                message: error.to_string(),
            })?;
        let replacement_id =
            IssueId::new(change.replacement_task_id.to_string()).map_err(|error| {
                PlanError::InvalidGraph {
                    message: error.to_string(),
                }
            })?;
        if !old.issues.contains_key(&old_id)
            || new.issues.contains_key(&old_id)
            || !new.issues.contains_key(&replacement_id)
        {
            return Err(PlanError::InvalidGraph {
                message: format!(
                    "invalid supersession {} -> {}",
                    change.task_id, change.replacement_task_id
                ),
            });
        }
        if change.reason.trim().is_empty() {
            return Err(PlanError::InvalidGraph {
                message: format!("supersession {} has no reason", change.task_id),
            });
        }
    }

    let mut discovered_ids = new_ids
        .difference(&old_ids)
        .map(|id| (*id).clone())
        .collect::<Vec<_>>();
    if state.tasks.is_empty() {
        discovered_ids = new_ids.iter().map(|id| (*id).clone()).collect();
    } else {
        let missing = new_ids
            .iter()
            .filter(|id| {
                !state
                    .tasks
                    .contains_key(&TaskId::new(id.to_string()).expect("issue ids are non-empty"))
            })
            .filter(|id| !discovered_ids.contains(id))
            .map(|id| (*id).clone())
            .collect::<Vec<_>>();
        discovered_ids.extend(missing);
    }
    discovered_ids.sort();
    discovered_ids.dedup();
    for change in &superseded {
        let replacement =
            IssueId::new(change.replacement_task_id.to_string()).map_err(|error| {
                PlanError::InvalidGraph {
                    message: error.to_string(),
                }
            })?;
        if !discovered_ids.contains(&replacement) {
            discovered_ids.push(replacement);
        }
    }
    discovered_ids.sort();

    let invalidated = old_ids
        .difference(&new_ids)
        .filter(|id| !superseded_old.contains(**id))
        .filter_map(|id| {
            let task_id = TaskId::new((*id).to_string()).ok()?;
            let task = state.tasks.get(&task_id)?;
            (!matches!(
                task.status,
                TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Invalidated
            ))
            .then_some(task_id)
        })
        .collect::<Vec<_>>();

    let discovered = discovered_ids
        .iter()
        .map(|id| TaskId::new(id.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| PlanError::InvalidGraph {
            message: error.to_string(),
        })?;
    let ready_candidates = new
        .issues
        .values()
        .filter(|issue| issue.kind == IssueKind::Task)
        .map(|issue| &issue.id)
        .filter(|id| {
            let task_id = TaskId::new(id.to_string()).expect("issue ids are non-empty");
            state
                .tasks
                .get(&task_id)
                .is_some_and(|task| task.status == TaskStatus::Pending)
                || discovered_ids.contains(id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let ready = new.ready_frontier(state, ready_candidates);

    Ok(PlanChange {
        discovered,
        ready,
        invalidated,
        superseded,
    })
}

fn parse_issue_records(contents: &str) -> Result<Vec<Issue>, PlanError> {
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(line_number, line)| {
            let record: IssueRecord =
                serde_json::from_str(line).map_err(|error| PlanError::InvalidGraph {
                    message: format!("invalid issue record at line {}: {error}", line_number + 1),
                })?;
            if record.record_kind != "issue" {
                return Err(PlanError::InvalidGraph {
                    message: format!("expected issue record at line {}", line_number + 1),
                });
            }
            Ok(record.issue)
        })
        .collect()
}

fn parse_edge_records(contents: &str) -> Result<Vec<Edge>, PlanError> {
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(line_number, line)| {
            let record: EdgeRecord =
                serde_json::from_str(line).map_err(|error| PlanError::InvalidGraph {
                    message: format!("invalid edge record at line {}: {error}", line_number + 1),
                })?;
            if record.record_kind != "edge" {
                return Err(PlanError::InvalidGraph {
                    message: format!("expected edge record at line {}", line_number + 1),
                });
            }
            Ok(record.edge)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::{Edge, EdgeType, IssueDraft, IssueId, IssueKind, Priority},
        runtime::{MutableState, RunId, RunTask, TaskId, TaskStatus},
    };

    use super::{PlanGraph, reconcile};

    fn issue(id: &str) -> crate::domain::Issue {
        crate::domain::Issue::from_draft(
            IssueId::new(id).expect("id"),
            IssueDraft {
                title: id.to_owned(),
                description: String::new(),
                priority: Priority::new(1).expect("priority"),
                kind: IssueKind::Task,
                created_at: "2026-09-29T00:00:00Z".to_owned(),
            },
        )
        .expect("issue")
    }

    fn edge(source: &str, target: &str) -> Edge {
        Edge::new(
            IssueId::new(source).expect("source"),
            IssueId::new(target).expect("target"),
            EdgeType::Blocks,
            "2026-09-29T00:00:00Z".to_owned(),
        )
        .expect("edge")
    }

    #[test]
    fn new_dependent_task_waits_for_its_blocker() {
        let old = PlanGraph::new(vec![issue("ag-aaaaaa")], vec![]).expect("old");
        let new = PlanGraph::new(
            vec![issue("ag-aaaaaa"), issue("ag-bbbbbb")],
            vec![edge("ag-bbbbbb", "ag-aaaaaa")],
        )
        .expect("new");
        let mut state = MutableState::new(RunId::new("run-1").expect("run"), "old").expect("state");
        state.tasks.insert(
            TaskId::new("ag-aaaaaa").expect("task"),
            RunTask {
                task_id: TaskId::new("ag-aaaaaa").expect("task"),
                status: TaskStatus::Pending,
                attempt: 0,
                active_attempt: None,
                worker: None,
                checkpoint: None,
                result: None,
                failure: None,
            },
        );

        let change = reconcile(&old, &new, &state, Vec::new(), "plan update").expect("change");
        assert_eq!(
            change.discovered,
            vec![TaskId::new("ag-bbbbbb").expect("task")]
        );
        assert_eq!(change.ready, vec![TaskId::new("ag-aaaaaa").expect("task")]);
    }
}
