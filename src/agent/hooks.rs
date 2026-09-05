use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{agent::prime, store::Store};

const MARKER_DIR: &str = "agent-stride/hooks";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    PreCompact,
    PostCompact,
    UserPromptSubmit,
}

impl HookEvent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "SessionStart" => Some(Self::SessionStart),
            "PreCompact" => Some(Self::PreCompact),
            "PostCompact" => Some(Self::PostCompact),
            "UserPromptSubmit" => Some(Self::UserPromptSubmit),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HookResult {
    pub event: String,
    pub refresh: bool,
    pub marker: PathBuf,
    pub context: Option<prime::PrimeContext>,
}

pub fn handle(
    store: &Store,
    workspace: &Path,
    event: HookEvent,
    actor: Option<&str>,
    session: &str,
) -> Result<HookResult, HookError> {
    if session.trim().is_empty() {
        return Err(HookError::InvalidSession);
    }
    let marker = marker_path(workspace, session)?;
    match event {
        HookEvent::SessionStart => Ok(HookResult {
            event: "SessionStart".to_owned(),
            refresh: true,
            marker,
            context: Some(prime::build(store, workspace, actor, None, None)?),
        }),
        HookEvent::PreCompact => Ok(HookResult {
            event: "PreCompact".to_owned(),
            refresh: false,
            marker,
            context: None,
        }),
        HookEvent::PostCompact => {
            if let Some(parent) = marker.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&marker, b"refresh\n")?;
            Ok(HookResult {
                event: "PostCompact".to_owned(),
                refresh: false,
                marker,
                context: None,
            })
        }
        HookEvent::UserPromptSubmit => {
            if !marker.is_file() {
                return Ok(HookResult {
                    event: "UserPromptSubmit".to_owned(),
                    refresh: false,
                    marker,
                    context: None,
                });
            }
            fs::remove_file(&marker)?;
            Ok(HookResult {
                event: "UserPromptSubmit".to_owned(),
                refresh: true,
                marker,
                context: Some(prime::build(store, workspace, actor, None, None)?),
            })
        }
    }
}

fn marker_path(workspace: &Path, session: &str) -> Result<PathBuf, HookError> {
    let root = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or(HookError::CacheDirectoryUnavailable)?;
    let safe_session = session
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    Ok(root.join(MARKER_DIR).join(format!(
        "{}-{safe_session}.refresh",
        workspace_hash(workspace)
    )))
}

fn workspace_hash(workspace: &Path) -> String {
    let digest = sha2::Sha256::digest(workspace.to_string_lossy().as_bytes());
    hex::encode(digest)[..16].to_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("session must not be empty")]
    InvalidSession,
    #[error("cache directory is unavailable")]
    CacheDirectoryUnavailable,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

use sha2::Digest;
