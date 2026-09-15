use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

use super::PlanError;

const PLAN_PATHSPEC: &str = ".tasks";

#[derive(Debug, Clone)]
pub struct GitRepository {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitFile {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitStatus {
    pub revision: String,
    pub dirty: bool,
    pub files: Vec<GitFile>,
}

impl GitRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn current_revision(&self) -> Result<String, PlanError> {
        let output = self.run(["rev-parse", "--verify", "HEAD"])?;
        let revision = output.trim().to_owned();
        if revision.is_empty() {
            return Err(PlanError::NoCommit);
        }
        Ok(revision)
    }

    pub fn status(&self) -> Result<GitStatus, PlanError> {
        let revision = self.current_revision()?;
        let output = self.run(["status", "--short", "--", PLAN_PATHSPEC])?;
        let files = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(parse_status_line)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(GitStatus {
            dirty: !files.is_empty(),
            files,
            revision,
        })
    }

    pub fn diff(&self, base: &str, head: Option<&str>) -> Result<String, PlanError> {
        validate_revision(base)?;
        if let Some(head) = head {
            validate_revision(head)?;
        }
        let range = head
            .map(|head| format!("{base}..{head}"))
            .unwrap_or_else(|| base.to_owned());
        self.run(["diff", "--no-ext-diff", &range, "--", PLAN_PATHSPEC])
    }

    pub fn read_file_at_revision(&self, revision: &str, path: &str) -> Result<String, PlanError> {
        validate_revision(revision)?;
        if path.is_empty() || path.starts_with('/') || path.contains("..") {
            return Err(PlanError::InvalidRevision {
                revision: path.to_owned(),
            });
        }
        self.run(["show", &format!("{revision}:{path}")])
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Result<String, PlanError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .map_err(|error| PlanError::Git {
                command: format!("git {}", args.join(" ")),
                message: error.to_string(),
            })?;
        if !output.status.success() {
            return Err(PlanError::Git {
                command: format!("git {}", args.join(" ")),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn parse_status_line(line: &str) -> Result<GitFile, PlanError> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 {
        return Err(PlanError::Git {
            command: "git status --short".to_owned(),
            message: format!("invalid status line `{line}`"),
        });
    }
    Ok(GitFile {
        status: line[..2].to_owned(),
        path: line[3..].to_owned(),
    })
}

fn validate_revision(revision: &str) -> Result<(), PlanError> {
    if revision.is_empty()
        || revision
            .chars()
            .any(|character| character.is_whitespace() || character == ';')
    {
        return Err(PlanError::InvalidRevision {
            revision: revision.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::GitRepository;

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git");
        assert!(status.success(), "git command failed: {args:?}");
    }

    #[test]
    fn reads_revision_and_detects_plan_drift() {
        let root = tempdir().expect("root");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "AgentStride"]);
        std::fs::create_dir(root.path().join(".tasks")).expect("tasks");
        std::fs::write(root.path().join(".tasks/issues.jsonl"), "one\n").expect("file");
        git(root.path(), &["add", ".tasks"]);
        git(root.path(), &["commit", "-qm", "plan"]);

        let repository = GitRepository::new(root.path());
        let revision = repository.current_revision().expect("revision");
        assert_eq!(revision.len(), 40);
        assert!(!repository.status().expect("status").dirty);

        std::fs::write(root.path().join(".tasks/issues.jsonl"), "two\n").expect("change");
        let status = repository.status().expect("dirty status");
        assert!(status.dirty);
        assert_eq!(status.files[0].path, ".tasks/issues.jsonl");
        assert_eq!(
            repository
                .read_file_at_revision(&revision, ".tasks/issues.jsonl")
                .expect("historical file"),
            "one\n"
        );
    }
}
