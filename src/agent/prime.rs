use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    domain::{Actor, Edge, Issue},
    store::{Checkpoint, JournalEvent, Lease, Store, StoreError},
};

use super::memory::MemoryRecord;

const DEFAULT_MEMORY_COUNT: usize = 8;
const DEFAULT_MEMORY_CHARS: usize = 4_000;
const RECENT_EVENT_LIMIT: usize = 10;

#[derive(Debug, Clone, Serialize)]
pub struct PrimeContext {
    pub schema_version: u8,
    pub actor: Option<String>,
    pub current_issue: Option<Issue>,
    pub checkpoint: Option<Checkpoint>,
    pub lease: Option<Lease>,
    pub dependencies: Vec<Edge>,
    pub dependents: Vec<Edge>,
    pub ready: Vec<Issue>,
    pub memories: Vec<MemoryRecord>,
    pub recent_events: Vec<JournalEvent>,
    pub custom_instructions: Option<String>,
    pub truncated: bool,
}

pub fn build(
    store: &Store,
    workspace: &Path,
    actor: Option<&str>,
    max_memory_count: Option<usize>,
    max_memory_chars: Option<usize>,
) -> Result<PrimeContext, StoreError> {
    let actor = actor.map(Actor::new).transpose()?;
    let current_issue = actor
        .as_ref()
        .map(|actor| current_issue(store, actor))
        .transpose()?
        .flatten();
    let checkpoint = current_issue
        .as_ref()
        .map(|issue| store.get_checkpoint(&issue.id))
        .transpose()?
        .flatten();
    let (lease, dependencies, dependents) = if let Some(issue) = &current_issue {
        let edges = store.list_edges()?;
        (
            store.get_lease(&issue.id)?,
            edges
                .iter()
                .filter(|edge| edge.source_id == issue.id && edge.edge_type.is_scheduling())
                .cloned()
                .collect(),
            edges
                .into_iter()
                .filter(|edge| edge.target_id == issue.id && edge.edge_type.is_scheduling())
                .collect(),
        )
    } else {
        (None, Vec::new(), Vec::new())
    };
    let mut memories = store
        .list_memories(None, None, false, Some(&now()))?
        .into_iter()
        .map(MemoryRecord::from)
        .collect::<Vec<_>>();
    memories.sort_by(|left, right| {
        right
            .importance
            .cmp(&left.importance)
            .then_with(|| right.confidence.cmp(&left.confidence))
            .then_with(|| left.key.cmp(&right.key))
    });
    let (memories, truncated) = cap_memories(
        memories,
        max_memory_count.unwrap_or(DEFAULT_MEMORY_COUNT),
        max_memory_chars.unwrap_or(DEFAULT_MEMORY_CHARS),
    );
    let ready = store.list_ready(&now())?;
    let recent_events = recent_events(store)?;
    let custom_instructions = load_instructions(workspace);

    Ok(PrimeContext {
        schema_version: 1,
        actor: actor.map(|value| value.as_str().to_owned()),
        current_issue,
        checkpoint,
        lease,
        dependencies,
        dependents,
        ready,
        memories,
        recent_events,
        custom_instructions,
        truncated,
    })
}

fn load_instructions(workspace: &Path) -> Option<String> {
    let project = workspace.join("PRIME.md");
    let global = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/agent-stride/PRIME.md"));
    [Some(project), global]
        .into_iter()
        .flatten()
        .find_map(|path| {
            fs::read_to_string(path)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.chars().take(4_000).collect())
        })
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting is infallible")
}

fn current_issue(store: &Store, actor: &Actor) -> Result<Option<Issue>, StoreError> {
    let mut issues = store
        .list_issues()?
        .into_iter()
        .filter(|issue| {
            issue.status == crate::domain::Status::InProgress
                && issue.assignee.as_ref() == Some(actor)
        })
        .collect::<Vec<_>>();
    issues.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    Ok(issues.into_iter().next())
}

fn recent_events(store: &Store) -> Result<Vec<JournalEvent>, StoreError> {
    store.list_recent_events(RECENT_EVENT_LIMIT)
}

fn cap_memories(
    memories: Vec<MemoryRecord>,
    max_count: usize,
    max_chars: usize,
) -> (Vec<MemoryRecord>, bool) {
    let mut used: usize = 0;
    let mut truncated = false;
    let mut selected = Vec::new();
    for memory in memories {
        if selected.len() >= max_count
            || used.saturating_add(memory.content.chars().count()) > max_chars
        {
            truncated = true;
            break;
        }
        used = used.saturating_add(memory.content.chars().count());
        selected.push(memory);
    }
    (selected, truncated)
}

pub fn render_human(context: &PrimeContext) -> String {
    let actor = context.actor.as_deref().unwrap_or("unassigned");
    let current = context
        .current_issue
        .as_ref()
        .map(|issue| format!("{} {}", issue.id, issue.title))
        .unwrap_or_else(|| "none".to_owned());
    let checkpoint = context
        .checkpoint
        .as_ref()
        .map(|value| format!("{} -> {}", value.summary, value.next_step))
        .unwrap_or_else(|| "none".to_owned());
    let lease = context
        .lease
        .as_ref()
        .map(|value| format!("{} until {}", value.owner.as_str(), value.until))
        .unwrap_or_else(|| "none".to_owned());
    let ready = context
        .ready
        .iter()
        .map(|issue| format!("{} {}", issue.id, issue.title))
        .collect::<Vec<_>>()
        .join("\n");
    let memories = context
        .memories
        .iter()
        .map(|memory| format!("{}: {}", memory.key, memory.content))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "actor: {actor}\ncurrent: {current}\nlease: {lease}\ncheckpoint: {checkpoint}\ndependencies: {}\ndependents: {}\nready:\n{}\nmemories:\n{}\ninstructions:\n{}\nrecent events: {}\ntruncated: {}",
        context.dependencies.len(),
        context.dependents.len(),
        if ready.is_empty() { "none" } else { &ready },
        if memories.is_empty() {
            "none"
        } else {
            &memories
        },
        context.custom_instructions.as_deref().unwrap_or("none"),
        context.recent_events.len(),
        context.truncated
    )
}

#[cfg(test)]
mod tests {
    use super::cap_memories;
    use crate::agent::memory::MemoryRecord;

    fn memory(key: &str, content: &str) -> MemoryRecord {
        MemoryRecord {
            key: key.to_owned(),
            content: content.to_owned(),
            scope: "workspace".to_owned(),
            source: "test".to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            updated_at: "2026-09-26T00:00:00Z".to_owned(),
            revision: 1,
            status: "active".to_owned(),
            importance: 3,
            confidence: 3,
            expires_at: None,
            last_used_at: None,
        }
    }

    #[test]
    fn memory_caps_keep_complete_records() {
        let (selected, truncated) =
            cap_memories(vec![memory("a", "123"), memory("b", "4567")], 10, 5);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].key, "a");
        assert!(truncated);
    }
}
