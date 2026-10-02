use std::{
    fs, io,
    path::{Path, PathBuf},
};

const BEGIN: &str = "<!-- AGENT-STRIDE:BEGIN -->";
const END: &str = "<!-- AGENT-STRIDE:END -->";
const AGENTS_BLOCK: &str = r#"<!-- AGENT-STRIDE:BEGIN -->

## AgentStride Workflow

- Start with `as prime --actor <actor>`.
- Choose work from `as ready --json`.
- Claim work before editing with `as claim <id> --actor <actor>`.
- Save resumable progress with `as checkpoint`.
- Close work only after tests pass with `as close`.
- Re-prime after context loss or session compaction.
- Use `send`, `inbox`, `thread`, and `reply` for agent coordination.

<!-- AGENT-STRIDE:END -->"#;

const SKILL: &str = r#"# agent-stride

Use `as` as the durable task and execution state for this repository.

1. Run `as prime --actor <actor>` before starting.
2. Claim one ready issue before modifying files.
3. Keep a checkpoint with the next concrete action and test evidence.
4. Close the issue only after verification.
5. Re-prime after context loss or session compaction.
6. Use message tools for coordination instead of chat-only TODOs.

The CLI is the source of truth for task state; do not replace durable work with
chat-only TODOs.
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    Install,
    Check,
    Remove,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SetupReport {
    pub root: PathBuf,
    pub agents_file: PathBuf,
    pub skill_file: PathBuf,
    pub changed: bool,
    pub drifted: bool,
    pub removed: bool,
}

pub fn apply(root: &Path, action: SetupAction) -> Result<SetupReport, SetupError> {
    let agents_file = root.join("AGENTS.md");
    let skill_file = root.join(".agents/skills/agent-stride/SKILL.md");
    match action {
        SetupAction::Install => {
            let changed = update_managed_block(&agents_file)?;
            let skill_changed = match fs::read(&skill_file) {
                Ok(existing) => existing != SKILL.as_bytes(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.into()),
            };
            if skill_changed {
                atomic_write(&skill_file, SKILL.as_bytes())?;
            }
            Ok(SetupReport {
                root: root.to_path_buf(),
                agents_file,
                skill_file,
                changed: changed || skill_changed,
                drifted: false,
                removed: false,
            })
        }
        SetupAction::Check => {
            let agents_ok = fs::read_to_string(&agents_file)
                .map(|value| value.contains(AGENTS_BLOCK))
                .unwrap_or(false);
            let skill_ok = fs::read(&skill_file)
                .map(|value| value == SKILL.as_bytes())
                .unwrap_or(false);
            Ok(SetupReport {
                root: root.to_path_buf(),
                agents_file,
                skill_file,
                changed: false,
                drifted: !(agents_ok && skill_ok),
                removed: false,
            })
        }
        SetupAction::Remove => {
            let changed = remove_managed_block(&agents_file)?;
            let removed = match fs::read(&skill_file) {
                Ok(value) if value == SKILL.as_bytes() => {
                    fs::remove_file(&skill_file)?;
                    true
                }
                _ => false,
            };
            Ok(SetupReport {
                root: root.to_path_buf(),
                agents_file,
                skill_file,
                changed,
                drifted: false,
                removed: changed || removed,
            })
        }
    }
}

fn update_managed_block(path: &Path) -> Result<bool, SetupError> {
    let original = fs::read_to_string(path).unwrap_or_default();
    let next = if let Some(start) = original.find(BEGIN) {
        let end = original[start..]
            .find(END)
            .ok_or(SetupError::MalformedManagedBlock)?;
        let end = start + end + END.len();
        format!("{}{}{}", &original[..start], AGENTS_BLOCK, &original[end..])
    } else if original.trim().is_empty() {
        format!("{AGENTS_BLOCK}\n")
    } else {
        format!("{}\n\n{}\n", original.trim_end(), AGENTS_BLOCK)
    };
    if next == original {
        return Ok(false);
    }
    atomic_write(path, next.as_bytes())?;
    Ok(true)
}

fn remove_managed_block(path: &Path) -> Result<bool, SetupError> {
    let original = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let Some(start) = original.find(BEGIN) else {
        return Ok(false);
    };
    let end = original[start..]
        .find(END)
        .ok_or(SetupError::MalformedManagedBlock)?;
    let end = start + end + END.len();
    let next = format!("{}{}", &original[..start], &original[end..]);
    atomic_write(path, next.as_bytes())?;
    Ok(true)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), SetupError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("agent-stride.tmp");
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("managed AGENT-STRIDE block is malformed")]
    MalformedManagedBlock,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_is_idempotent_and_preserves_user_text() {
        let root = tempdir().expect("tempdir");
        let agents = root.path().join("AGENTS.md");
        fs::write(&agents, "user rules\n").expect("write");
        let first = apply(root.path(), SetupAction::Install).expect("install");
        let second = apply(root.path(), SetupAction::Install).expect("install");
        assert!(first.changed);
        assert!(!second.changed);
        let content = fs::read_to_string(agents).expect("read");
        assert!(content.contains("user rules"));
        assert_eq!(content.matches(BEGIN).count(), 1);
    }

    #[test]
    fn remove_only_removes_owned_files() {
        let root = tempdir().expect("tempdir");
        apply(root.path(), SetupAction::Install).expect("install");
        let report = apply(root.path(), SetupAction::Remove).expect("remove");
        assert!(report.removed);
        assert!(
            !root
                .path()
                .join(".agents/skills/agent-stride/SKILL.md")
                .exists()
        );
    }
}
