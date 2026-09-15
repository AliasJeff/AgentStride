use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    domain::{Edge, Issue},
    store::Store,
};

use super::PlanError;

const PLAN_DIR: &str = ".tasks";
const ISSUES_FILE: &str = "issues.jsonl";
const EDGES_FILE: &str = "edges.jsonl";
const METADATA_FILE: &str = "metadata.json";

#[derive(Debug, Clone, Serialize)]
pub struct PlanSnapshot {
    pub root: PathBuf,
    pub issues: usize,
    pub edges: usize,
}

#[derive(Debug, Serialize)]
struct IssueRecord<'a> {
    record_kind: &'static str,
    #[serde(flatten)]
    issue: &'a Issue,
}

#[derive(Debug, Serialize)]
struct EdgeRecord<'a> {
    record_kind: &'static str,
    #[serde(flatten)]
    edge: &'a Edge,
}

#[derive(Debug, Serialize)]
struct PlanMetadata {
    schema_version: u32,
    format: &'static str,
}

pub fn export_snapshot(store: &Store, project_root: &Path) -> Result<PlanSnapshot, PlanError> {
    if !project_root.is_dir() {
        return Err(PlanError::InvalidRoot {
            path: project_root.to_path_buf(),
        });
    }

    let plan_dir = project_root.join(PLAN_DIR);
    fs::create_dir_all(&plan_dir).map_err(|source| PlanError::Io {
        path: plan_dir.clone(),
        source,
    })?;

    let issues = store.list_issues()?;
    let edges = store.list_edges()?;

    write_jsonl(
        &plan_dir.join(ISSUES_FILE),
        issues.iter().map(|issue| IssueRecord {
            record_kind: "issue",
            issue,
        }),
    )?;
    write_jsonl(
        &plan_dir.join(EDGES_FILE),
        edges.iter().map(|edge| EdgeRecord {
            record_kind: "edge",
            edge,
        }),
    )?;
    write_json(
        &plan_dir.join(METADATA_FILE),
        &PlanMetadata {
            schema_version: 1,
            format: "agent-stride-plan",
        },
    )?;

    Ok(PlanSnapshot {
        root: plan_dir,
        issues: issues.len(),
        edges: edges.len(),
    })
}

fn write_jsonl<T, I>(path: &Path, records: I) -> Result<(), PlanError>
where
    T: Serialize,
    I: IntoIterator<Item = T>,
{
    let file = File::create(path).map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    for record in records {
        serde_json::to_writer(&mut writer, &record)?;
        writer.write_all(b"\n").map_err(|source| PlanError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    writer.flush().map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), PlanError> {
    let file = File::create(path).map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n").map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    writer.flush().map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::export_snapshot;
    use crate::{
        domain::{IssueDraft, IssueId, Priority},
        store::Store,
    };

    #[test]
    fn repeated_exports_are_byte_stable() {
        let root = tempdir().expect("root");
        let database = root.path().join("agent-stride.db");
        let mut store = Store::open(&database).expect("open");
        store.migrate().expect("migrate");
        store
            .insert_draft(
                IssueId::new("ag-12345678").expect("id"),
                IssueDraft {
                    title: "Plan".to_owned(),
                    description: "stable".to_owned(),
                    priority: Priority::new(1).expect("priority"),
                    kind: crate::domain::IssueKind::Task,
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
            )
            .expect("insert");

        export_snapshot(&store, root.path()).expect("first export");
        let first = fs::read(root.path().join(".tasks/issues.jsonl")).expect("first bytes");
        export_snapshot(&store, root.path()).expect("second export");
        let second = fs::read(root.path().join(".tasks/issues.jsonl")).expect("second bytes");

        assert_eq!(first, second);
    }
}
