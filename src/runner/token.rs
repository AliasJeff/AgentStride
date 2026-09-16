use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    domain::{Actor, IssueId, Revision},
    store::{Store, StoreError},
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimToken {
    pub workspace_id: String,
    pub issue_id: IssueId,
    pub actor: Actor,
    pub revision: Revision,
    pub lease_until: String,
    pub signature: String,
}

impl ClaimToken {
    pub fn new(
        workspace_id: String,
        issue_id: IssueId,
        actor: Actor,
        revision: Revision,
        lease_until: String,
    ) -> Result<Self, TokenError> {
        if workspace_id.trim().is_empty() || lease_until.trim().is_empty() {
            return Err(TokenError::Invalid("empty token claim".to_owned()));
        }
        let mut token = Self {
            workspace_id,
            issue_id,
            actor,
            revision,
            lease_until,
            signature: String::new(),
        };
        token.signature = token.sign()?;
        Ok(token)
    }

    pub fn encode(&self) -> Result<String, TokenError> {
        let claims = serde_json::json!({
            "workspace_id": self.workspace_id,
            "issue_id": self.issue_id,
            "actor": self.actor,
            "revision": self.revision,
            "lease_until": self.lease_until,
        });
        let payload = hex::encode(serde_json::to_vec(&claims)?);
        Ok(format!("{payload}.{}", self.signature))
    }

    pub fn decode(value: &str) -> Result<Self, TokenError> {
        let (payload, signature) = value.split_once('.').ok_or_else(|| {
            TokenError::Invalid("token must contain payload and signature".to_owned())
        })?;
        let bytes = hex::decode(payload)
            .map_err(|_| TokenError::Invalid("token payload is not hex".to_owned()))?;
        let claims: Claims = serde_json::from_slice(&bytes)?;
        let token = Self {
            workspace_id: claims.workspace_id,
            issue_id: claims.issue_id,
            actor: claims.actor,
            revision: claims.revision,
            lease_until: claims.lease_until,
            signature: signature.to_owned(),
        };
        if token.sign()? != token.signature {
            return Err(TokenError::SignatureMismatch);
        }
        Ok(token)
    }

    pub fn verify(
        &self,
        workspace_id: &str,
        issue_id: &IssueId,
        actor: &Actor,
        store: &Store,
    ) -> Result<(), TokenError> {
        if self.workspace_id != workspace_id {
            return Err(TokenError::WorkspaceMismatch);
        }
        if &self.issue_id != issue_id || &self.actor != actor {
            return Err(TokenError::OwnerMismatch);
        }
        let issue = store
            .get_issue(issue_id)
            .map_err(TokenError::Store)?
            .ok_or_else(|| TokenError::Invalid("issue no longer exists".to_owned()))?;
        if issue.revision != self.revision {
            return Err(TokenError::RevisionMismatch);
        }
        let lease = store
            .get_lease(issue_id)
            .map_err(TokenError::Store)?
            .ok_or_else(|| TokenError::Expired)?;
        if lease.owner != *actor || lease.until != self.lease_until {
            return Err(TokenError::OwnerMismatch);
        }
        if lease.until <= now_rfc3339() {
            return Err(TokenError::Expired);
        }
        Ok(())
    }

    fn sign(&self) -> Result<String, TokenError> {
        let mut mac = HmacSha256::new_from_slice(self.workspace_id.as_bytes())
            .map_err(|_| TokenError::Invalid("invalid signing key".to_owned()))?;
        mac.update(&canonical_claims(self)?);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    workspace_id: String,
    issue_id: IssueId,
    actor: Actor,
    revision: Revision,
    lease_until: String,
}

fn canonical_claims(token: &ClaimToken) -> Result<Vec<u8>, TokenError> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "workspace_id": token.workspace_id,
        "issue_id": token.issue_id,
        "actor": token.actor,
        "revision": token.revision,
        "lease_until": token.lease_until,
    }))?)
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting is infallible")
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("invalid claim token: {0}")]
    Invalid(String),
    #[error("claim token signature mismatch")]
    SignatureMismatch,
    #[error("claim token workspace mismatch")]
    WorkspaceMismatch,
    #[error("claim token owner mismatch")]
    OwnerMismatch,
    #[error("claim token revision mismatch")]
    RevisionMismatch,
    #[error("claim token expired")]
    Expired,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}
