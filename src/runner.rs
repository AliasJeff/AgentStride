pub mod policy;
pub mod token;

use std::process::Command;

use crate::{
    domain::{Actor, IssueId},
    store::{Lease, Store, StoreError},
};

use self::{
    policy::ExecutionPolicy,
    token::{ClaimToken, TokenError},
};

#[derive(Debug, serde::Serialize)]
pub struct RunResult {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn issue_token(
    store: &Store,
    issue_id: &IssueId,
    actor: &Actor,
) -> Result<String, RunnerError> {
    let issue = store
        .get_issue(issue_id)?
        .ok_or_else(|| StoreError::IssueNotFound {
            id: issue_id.to_string(),
        })?;
    let lease = store
        .get_lease(issue_id)?
        .ok_or_else(|| StoreError::LeaseNotOwned {
            id: issue_id.to_string(),
            actor: actor.as_str().to_owned(),
        })?;
    validate_lease(&lease, actor)?;
    let workspace_id = store.ensure_workspace_id()?;
    Ok(ClaimToken::new(
        workspace_id,
        issue.id,
        actor.clone(),
        issue.revision,
        lease.until,
    )?
    .encode()?)
}

pub fn run_command(
    store: &Store,
    issue_id: &IssueId,
    actor: &Actor,
    raw_token: &str,
    command: &str,
    args: &[String],
) -> Result<RunResult, RunnerError> {
    let workspace_id = store.ensure_workspace_id()?;
    let token = ClaimToken::decode(raw_token)?;
    token.verify(&workspace_id, issue_id, actor, store)?;
    ExecutionPolicy::default().check(command)?;

    let mut child = Command::new(command);
    child
        .args(args)
        .env("AGENT_STRIDE_CLAIM_TOKEN", raw_token)
        .env("AGENT_STRIDE_ISSUE", issue_id.as_str())
        .env("AGENT_STRIDE_ACTOR", actor.as_str());
    let output = child.output()?;
    Ok(RunResult {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn validate_lease(lease: &Lease, actor: &Actor) -> Result<(), RunnerError> {
    if lease.owner != *actor {
        return Err(RunnerError::Token(TokenError::OwnerMismatch));
    }
    if lease.until <= now_rfc3339() {
        return Err(RunnerError::Token(TokenError::Expired));
    }
    Ok(())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting is infallible")
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error("command `{command}` is not allowed by the runner policy")]
    CommandDenied { command: String },
    #[error("failed to start command: {0}")]
    Io(#[from] std::io::Error),
}

impl RunnerError {
    pub fn command_denied(command: &str) -> Self {
        Self::CommandDenied {
            command: command.to_owned(),
        }
    }
}
