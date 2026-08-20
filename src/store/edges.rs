use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    domain::{Edge, EdgeType, IssueId},
    graph::would_create_cycle,
};

use super::{Store, StoreError, journal};

impl Store {
    pub fn insert_edge(&mut self, edge: Edge) -> Result<Edge, StoreError> {
        let transaction = self.connection.transaction()?;
        let source_exists = issue_exists(&transaction, &edge.source_id)?;
        let target_exists = issue_exists(&transaction, &edge.target_id)?;
        if !source_exists {
            return Err(StoreError::IssueNotFound {
                id: edge.source_id.to_string(),
            });
        }
        if !target_exists {
            return Err(StoreError::IssueNotFound {
                id: edge.target_id.to_string(),
            });
        }

        let duplicate: Option<String> = transaction
            .query_row(
                "SELECT edge_type FROM edges WHERE source_id = ?1 AND target_id = ?2",
                params![edge.source_id.as_str(), edge.target_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(StoreError::DuplicateDependency {
                source_id: edge.source_id.to_string(),
                target_id: edge.target_id.to_string(),
            });
        }

        let existing = load_edges(&transaction)?;
        if edge.edge_type.is_scheduling() {
            would_create_cycle(&edge.source_id, &edge.target_id, existing)?;
        }

        transaction.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.source_id.as_str(),
                edge.target_id.as_str(),
                edge.edge_type.as_str(),
                &edge.created_at,
            ],
        )?;
        journal::append_event(
            &transaction,
            crate::domain::Operation::EdgeCreated,
            Some(&edge.source_id),
            None,
            &edge.created_at,
            None,
            journal::snapshot(&edge)?,
        )?;
        transaction.commit()?;
        Ok(edge)
    }

    pub fn remove_edge(
        &mut self,
        source_id: &IssueId,
        target_id: &IssueId,
        removed_at: &str,
    ) -> Result<bool, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let edge = transaction
            .query_row(
                "SELECT source_id, target_id, edge_type, created_at
                 FROM edges
                 WHERE source_id = ?1 AND target_id = ?2",
                params![source_id.as_str(), target_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .map(|(source, target, edge_type, created_at)| {
                Edge::new(
                    IssueId::new(source)?,
                    IssueId::new(target)?,
                    EdgeType::parse(&edge_type)?,
                    created_at,
                )
            })
            .transpose()?;
        let Some(edge) = edge else {
            transaction.commit()?;
            return Ok(false);
        };
        let changed = transaction.execute(
            "DELETE FROM edges WHERE source_id = ?1 AND target_id = ?2",
            params![source_id.as_str(), target_id.as_str()],
        )?;
        if changed == 1 {
            journal::append_event(
                &transaction,
                crate::domain::Operation::EdgeRemoved,
                Some(&edge.source_id),
                None,
                removed_at,
                None,
                journal::snapshot(&edge)?,
            )?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn list_edges(&self) -> Result<Vec<Edge>, StoreError> {
        load_edges(&self.connection)
    }
}

fn issue_exists(connection: &rusqlite::Transaction<'_>, id: &IssueId) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM issues WHERE id = ?1)",
        params![id.as_str()],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_edges(connection: &rusqlite::Connection) -> Result<Vec<Edge>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT source_id, target_id, edge_type, created_at
         FROM edges
         ORDER BY source_id, target_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    rows.map(|row| {
        let (source, target, edge_type, created_at) = row?;
        Ok(Edge::new(
            IssueId::new(source)?,
            IssueId::new(target)?,
            EdgeType::parse(&edge_type)?,
            created_at,
        )?)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::domain::{Edge, EdgeType, IssueDraft, IssueId, Priority};
    use proptest::prelude::*;

    fn id(value: &str) -> IssueId {
        IssueId::new(value).expect("valid issue id")
    }

    fn store_with_issues() -> Store {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        for value in ["ag-aaaaaa", "ag-bbbbbb", "ag-cccccc", "ag-dddddd"] {
            store
                .insert_draft(
                    id(value),
                    IssueDraft {
                        title: value.to_owned(),
                        description: String::new(),
                        priority: Priority::new(1).expect("valid priority"),
                        kind: crate::domain::IssueKind::Task,
                        created_at: "2026-09-26T00:00:00Z".to_owned(),
                    },
                )
                .expect("insert issue");
        }
        store
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
    fn insert_edge_rejects_cycle_without_partial_write() {
        let mut store = store_with_issues();
        store
            .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks))
            .expect("insert first edge");

        assert!(
            store
                .insert_edge(edge("ag-aaaaaa", "ag-bbbbbb", EdgeType::Blocks))
                .is_err()
        );

        let edges = store.list_edges().expect("list edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source_id.as_str(), "ag-bbbbbb");
    }

    #[test]
    fn related_edge_does_not_participate_in_cycle_detection() {
        let mut store = store_with_issues();
        store
            .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Related))
            .expect("insert related edge");
        store
            .insert_edge(edge("ag-aaaaaa", "ag-bbbbbb", EdgeType::Related))
            .expect("insert reverse related edge");

        assert_eq!(store.list_edges().expect("list edges").len(), 2);
    }

    proptest! {
        #[test]
        fn rejected_cycle_never_changes_edge_state(
            extra_edges in prop::array::uniform5(any::<bool>()),
        ) {
            let mut store = store_with_issues();
            store
                .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks))
                .expect("insert base edge");

            let candidates = [
                ("ag-cccccc", "ag-aaaaaa"),
                ("ag-cccccc", "ag-bbbbbb"),
                ("ag-dddddd", "ag-aaaaaa"),
                ("ag-dddddd", "ag-bbbbbb"),
                ("ag-dddddd", "ag-cccccc"),
            ];
            for ((source, target), enabled) in candidates.into_iter().zip(extra_edges) {
                if enabled {
                    store
                        .insert_edge(edge(source, target, EdgeType::Blocks))
                        .expect("extra DAG edge");
                }
            }

            let before = store.list_edges().expect("read edges");
            let result = store.insert_edge(edge(
                "ag-aaaaaa",
                "ag-bbbbbb",
                EdgeType::Blocks,
            ));
            prop_assert!(result.is_err());
            prop_assert_eq!(store.list_edges().expect("read edges"), before);
        }
    }
}
