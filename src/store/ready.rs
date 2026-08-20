use rusqlite::{Connection, params};

use crate::domain::Issue;

use super::{Store, StoreError, issues::decode_issue};

impl Store {
    pub fn list_ready(&self, now: &str) -> Result<Vec<Issue>, StoreError> {
        query_ready(&self.connection, now)
    }

    pub fn list_blocked(&self) -> Result<Vec<Issue>, StoreError> {
        query_issues(
            &self.connection,
            r#"
            WITH RECURSIVE blocked(id) AS (
                SELECT e.source_id
                FROM edges e
                JOIN issues target ON target.id = e.target_id
                WHERE e.edge_type = 'blocks'
                  AND target.status <> 'closed'
                UNION
                SELECT e.source_id
                FROM edges e
                JOIN blocked b ON b.id = e.target_id
                WHERE e.edge_type = 'parent_child'
            )
            SELECT i.id, i.title, i.description, i.kind, i.status, i.priority,
                   i.assignee, i.created_at, i.updated_at, i.defer_until,
                   i.close_reason, i.revision
            FROM issues i
            JOIN blocked b ON b.id = i.id
            WHERE i.status <> 'closed'
            ORDER BY i.priority, i.created_at, i.id
            "#,
            [],
        )
    }
}

pub(crate) fn query_ready(connection: &Connection, now: &str) -> Result<Vec<Issue>, StoreError> {
    query_issues(
        connection,
        r#"
        WITH RECURSIVE blocked(id) AS (
            SELECT e.source_id
            FROM edges e
            JOIN issues target ON target.id = e.target_id
            WHERE e.edge_type = 'blocks'
              AND target.status <> 'closed'
            UNION
            SELECT e.source_id
            FROM edges e
            JOIN blocked b ON b.id = e.target_id
            WHERE e.edge_type = 'parent_child'
        )
        SELECT i.id, i.title, i.description, i.kind, i.status, i.priority,
               i.assignee, i.created_at, i.updated_at, i.defer_until,
               i.close_reason, i.revision
        FROM issues i
        WHERE i.status = 'open'
          AND i.kind = 'task'
          AND (i.defer_until IS NULL OR i.defer_until <= ?1)
          AND NOT EXISTS (
              SELECT 1 FROM blocked b WHERE b.id = i.id
          )
        ORDER BY i.priority, i.created_at, i.id
        "#,
        params![now],
    )
}

pub(crate) fn query_issues(
    connection: &Connection,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<Issue>, StoreError> {
    let mut statement = connection.prepare(query)?;
    let rows = statement.query_map(parameters, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, u8>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, u64>(11)?,
        ))
    })?;

    rows.map(|row| row.map_err(StoreError::from).and_then(decode_issue))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::domain::{Edge, EdgeType, IssueDraft, IssueId, Priority, Status};
    use proptest::prelude::*;

    fn id(value: &str) -> IssueId {
        IssueId::new(value).expect("valid issue id")
    }

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");
        for value in ["ag-aaaaaa", "ag-bbbbbb", "ag-cccccc"] {
            store
                .insert_draft(
                    id(value),
                    IssueDraft {
                        title: value.to_owned(),
                        description: String::new(),
                        priority: Priority::new(1).expect("valid priority"),
                        kind: crate::domain::IssueKind::Task,
                        created_at: value.to_owned(),
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
    fn open_blocker_removes_dependent_from_ready() {
        let mut store = store();
        store
            .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks))
            .expect("insert edge");

        let ready = store
            .list_ready("2026-09-26T00:00:00Z")
            .expect("list ready");
        assert_eq!(
            ready
                .iter()
                .map(|issue| issue.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ag-aaaaaa", "ag-cccccc"]
        );
    }

    #[test]
    fn closing_blocker_releases_dependent() {
        let mut store = store();
        store
            .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks))
            .expect("insert edge");
        store
            .close_issue(
                &id("ag-aaaaaa"),
                "done".to_owned(),
                "2026-09-26T00:01:00Z".to_owned(),
                None,
            )
            .expect("close blocker");

        let ready = store
            .list_ready("2026-09-26T00:01:00Z")
            .expect("list ready");
        assert!(ready.iter().any(|issue| issue.id == id("ag-bbbbbb")));
    }

    #[test]
    fn blocked_parent_propagates_to_child() {
        let mut store = store();
        store
            .insert_edge(edge("ag-bbbbbb", "ag-aaaaaa", EdgeType::Blocks))
            .expect("insert blocker");
        store
            .insert_edge(edge("ag-cccccc", "ag-bbbbbb", EdgeType::ParentChild))
            .expect("insert parent edge");

        let blocked = store.list_blocked().expect("list blocked");
        assert_eq!(
            blocked
                .iter()
                .map(|issue| issue.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ag-bbbbbb", "ag-cccccc"]
        );
        assert_eq!(
            store
                .get_issue(&id("ag-cccccc"))
                .expect("read child")
                .expect("child exists")
                .status,
            Status::Open
        );
    }

    proptest! {
        #[test]
        fn ready_query_matches_a_small_reference_model(
            edge_flags in prop::array::uniform6(any::<bool>()),
            closed in prop::array::uniform4(any::<bool>()),
        ) {
            let ids = ["ag-aaaaaa", "ag-bbbbbb", "ag-cccccc", "ag-dddddd"];
            let pairs = [
                (1usize, 0usize),
                (2, 0),
                (2, 1),
                (3, 0),
                (3, 1),
                (3, 2),
            ];
            let mut store = store();
            store
                .insert_draft(
                    id("ag-dddddd"),
                    IssueDraft {
                        title: "ag-dddddd".to_owned(),
                        description: String::new(),
                        priority: Priority::new(1).expect("priority"),
                        kind: crate::domain::IssueKind::Task,
                        created_at: "ag-dddddd".to_owned(),
                    },
                )
                .expect("insert fourth issue");

            for (index, should_close) in closed.iter().copied().enumerate() {
                if should_close {
                    store
                        .close_issue(
                            &id(ids[index]),
                            "done".to_owned(),
                            "2026-09-26T00:01:00Z".to_owned(),
                            None,
                        )
                        .expect("close issue");
                }
            }

            let mut edges = Vec::new();
            for (index, ((source, target), enabled)) in
                pairs.into_iter().zip(edge_flags).enumerate()
            {
                if !enabled {
                    continue;
                }
                let edge_type = if index % 2 == 0 {
                    EdgeType::Blocks
                } else {
                    EdgeType::ParentChild
                };
                let edge = edge(ids[source], ids[target], edge_type);
                store.insert_edge(edge.clone()).expect("insert DAG edge");
                edges.push(edge);
            }

            let mut blocked = [false; 4];
            loop {
                let mut changed = false;
                for edge in &edges {
                    let source = ids
                        .iter()
                        .position(|value| *value == edge.source_id.as_str())
                        .expect("source index");
                    let target = ids
                        .iter()
                        .position(|value| *value == edge.target_id.as_str())
                        .expect("target index");
                    let should_block = match edge.edge_type {
                        EdgeType::Blocks => !closed[target],
                        EdgeType::ParentChild => blocked[target],
                        EdgeType::Related => false,
                    };
                    if should_block && !blocked[source] {
                        blocked[source] = true;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }

            let expected = ids
                .iter()
                .enumerate()
                .filter(|(index, _)| !closed[*index] && !blocked[*index])
                .map(|(_, value)| (*value).to_owned())
                .collect::<Vec<_>>();
            let actual = store
                .list_ready("2026-09-26T00:02:00Z")
                .expect("list ready")
                .into_iter()
                .map(|issue| issue.id.to_string())
                .collect::<Vec<_>>();

            prop_assert_eq!(actual, expected);
        }
    }
}
