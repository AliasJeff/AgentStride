use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::DomainError;

const ID_PREFIX: &str = "ag-";
const MIN_VISIBLE_DIGEST_LENGTH: usize = 6;
const MAX_VISIBLE_DIGEST_LENGTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueId(String);

impl IssueId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if is_valid_id(&value) {
            Ok(Self(value))
        } else {
            Err(DomainError::InvalidIssueId { value })
        }
    }

    pub fn generate(
        workspace_id: &str,
        title: &str,
        description: &str,
        created_at: &str,
        nonce: u32,
    ) -> Self {
        let digest = generate_digest(workspace_id, title, description, created_at, nonce);
        Self(visible_id(&digest, MIN_VISIBLE_DIGEST_LENGTH))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn generate_digest(
    workspace_id: &str,
    title: &str,
    description: &str,
    created_at: &str,
    nonce: u32,
) -> String {
    let mut hasher = Sha256::new();
    for field in [
        "agent-stride-id-v1",
        workspace_id,
        title,
        description,
        created_at,
        &nonce.to_string(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub fn visible_id(digest: &str, length: usize) -> String {
    let length = length.clamp(MIN_VISIBLE_DIGEST_LENGTH, MAX_VISIBLE_DIGEST_LENGTH);
    format!("{ID_PREFIX}{}", &digest[..length.min(digest.len())])
}

fn is_valid_id(value: &str) -> bool {
    let Some(digest) = value.strip_prefix(ID_PREFIX) else {
        return false;
    };

    (MIN_VISIBLE_DIGEST_LENGTH..=MAX_VISIBLE_DIGEST_LENGTH).contains(&digest.len())
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{IssueId, generate_digest, visible_id};

    #[test]
    fn generated_id_is_stable() {
        let first = IssueId::generate(
            "workspace",
            "Title",
            "Description",
            "2026-01-01T00:00:00Z",
            0,
        );
        let second = IssueId::generate(
            "workspace",
            "Title",
            "Description",
            "2026-01-01T00:00:00Z",
            0,
        );

        assert_eq!(first, second);
        assert_eq!(first.as_str().len(), 9);
    }

    #[test]
    fn workspace_changes_digest() {
        let first = generate_digest("one", "Title", "", "now", 0);
        let second = generate_digest("two", "Title", "", "now", 0);

        assert_ne!(first, second);
    }

    #[test]
    fn visible_id_extends_for_collision_resolution() {
        let digest = generate_digest("workspace", "Title", "", "now", 0);
        let short = visible_id(&digest, 6);
        let long = visible_id(&digest, 8);

        assert_eq!(short.len(), 9);
        assert_eq!(long.len(), 11);
        assert!(long.starts_with(&short));
    }
}
