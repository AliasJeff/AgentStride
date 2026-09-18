use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::{AttemptId, RunId, TaskId};
use crate::domain::Actor;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTaskToken {
    pub workspace_id: String,
    pub run_id: RunId,
    pub durable_task_id: String,
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub worker: Actor,
    pub history_version: u64,
    pub lease_until: String,
    pub signature: String,
}

impl RuntimeTaskToken {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace_id: String,
        run_id: RunId,
        durable_task_id: String,
        task_id: TaskId,
        attempt_id: AttemptId,
        worker: Actor,
        history_version: u64,
        lease_until: String,
    ) -> Result<Self, RuntimeTokenError> {
        if workspace_id.trim().is_empty()
            || durable_task_id.trim().is_empty()
            || lease_until.trim().is_empty()
        {
            return Err(RuntimeTokenError::Invalid(
                "empty runtime task token claim".to_owned(),
            ));
        }
        let mut token = Self {
            workspace_id,
            run_id,
            durable_task_id,
            task_id,
            attempt_id,
            worker,
            history_version,
            lease_until,
            signature: String::new(),
        };
        token.signature = token.sign()?;
        Ok(token)
    }

    pub fn encode(&self) -> Result<String, RuntimeTokenError> {
        let payload = hex::encode(serde_json::to_vec(&self.claims())?);
        Ok(format!("{payload}.{}", self.signature))
    }

    pub fn decode(value: &str) -> Result<Self, RuntimeTokenError> {
        let (payload, signature) = value
            .split_once('.')
            .ok_or_else(|| RuntimeTokenError::Invalid("missing token separator".to_owned()))?;
        let bytes = hex::decode(payload)
            .map_err(|_| RuntimeTokenError::Invalid("token payload is not hex".to_owned()))?;
        let claims: Claims = serde_json::from_slice(&bytes)?;
        let token = Self {
            workspace_id: claims.workspace_id,
            run_id: claims.run_id,
            durable_task_id: claims.durable_task_id,
            task_id: claims.task_id,
            attempt_id: claims.attempt_id,
            worker: claims.worker,
            history_version: claims.history_version,
            lease_until: claims.lease_until,
            signature: signature.to_owned(),
        };
        if token.sign()? != token.signature {
            return Err(RuntimeTokenError::SignatureMismatch);
        }
        Ok(token)
    }

    pub fn verify_signature(&self, workspace_id: &str) -> Result<(), RuntimeTokenError> {
        if self.workspace_id != workspace_id {
            return Err(RuntimeTokenError::WorkspaceMismatch);
        }
        if self.sign()? != self.signature {
            return Err(RuntimeTokenError::SignatureMismatch);
        }
        Ok(())
    }

    fn claims(&self) -> Claims {
        Claims {
            workspace_id: self.workspace_id.clone(),
            run_id: self.run_id.clone(),
            durable_task_id: self.durable_task_id.clone(),
            task_id: self.task_id.clone(),
            attempt_id: self.attempt_id.clone(),
            worker: self.worker.clone(),
            history_version: self.history_version,
            lease_until: self.lease_until.clone(),
        }
    }

    fn sign(&self) -> Result<String, RuntimeTokenError> {
        let mut mac = HmacSha256::new_from_slice(self.workspace_id.as_bytes())
            .map_err(|_| RuntimeTokenError::Invalid("invalid signing key".to_owned()))?;
        mac.update(&serde_json::to_vec(&self.claims())?);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    workspace_id: String,
    run_id: RunId,
    durable_task_id: String,
    task_id: TaskId,
    attempt_id: AttemptId,
    worker: Actor,
    history_version: u64,
    lease_until: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeTokenError {
    #[error("invalid runtime task token: {0}")]
    Invalid(String),
    #[error("runtime task token signature mismatch")]
    SignatureMismatch,
    #[error("runtime task token workspace mismatch")]
    WorkspaceMismatch,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::RuntimeTaskToken;
    use crate::{
        domain::Actor,
        runtime::{AttemptId, RunId, TaskId},
    };

    fn token() -> RuntimeTaskToken {
        RuntimeTaskToken::new(
            "workspace".to_owned(),
            RunId::new("run-1").expect("run"),
            "run-1-task-1".to_owned(),
            TaskId::new("task-1").expect("task"),
            AttemptId::new("attempt-1").expect("attempt"),
            Actor::new("worker-a").expect("actor"),
            4,
            "2026-09-29T00:01:00Z".to_owned(),
        )
        .expect("token")
    }

    #[test]
    fn token_round_trips_and_verifies() {
        let token = token();
        let encoded = token.encode().expect("encode");
        let decoded = RuntimeTaskToken::decode(&encoded).expect("decode");
        assert_eq!(decoded, token);
        decoded
            .verify_signature("workspace")
            .expect("signature should verify");
    }

    #[test]
    fn token_tampering_is_rejected() {
        let token = token();
        let mut encoded = token.encode().expect("encode");
        encoded.push('0');
        assert!(RuntimeTaskToken::decode(&encoded).is_err());
    }
}
