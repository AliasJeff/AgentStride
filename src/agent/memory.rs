use serde::{Deserialize, Serialize};

use crate::store::{Memory, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub key: String,
    pub content: String,
    pub scope: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub status: String,
    pub importance: u8,
    pub confidence: u8,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
}

impl From<Memory> for MemoryRecord {
    fn from(memory: Memory) -> Self {
        Self {
            key: memory.key,
            content: memory.content,
            scope: memory.scope,
            source: memory.source,
            created_at: memory.created_at,
            updated_at: memory.updated_at,
            revision: memory.revision,
            status: memory.status,
            importance: memory.importance,
            confidence: memory.confidence,
            expires_at: memory.expires_at,
            last_used_at: memory.last_used_at,
        }
    }
}

pub fn derive_key(content: &str) -> Option<String> {
    let mut key = String::new();
    let mut pending_separator = false;

    for character in content.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !key.is_empty() {
                key.push('-');
            }
            key.push(character);
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }

    if key.is_empty() {
        return None;
    }

    let key = key.split('-').take(8).collect::<Vec<_>>().join("-");
    Some(key.chars().take(60).collect())
}

pub fn resolve_key(key: Option<String>, content: &str) -> Result<String, StoreError> {
    if content.trim().is_empty() {
        return Err(StoreError::InvalidMemory {
            message: "memory content must not be empty".to_owned(),
        });
    }

    match key {
        Some(key) if key.trim().is_empty() => Err(StoreError::InvalidMemory {
            message: "memory key must not be empty".to_owned(),
        }),
        Some(key) => Ok(key),
        None => derive_key(content).ok_or_else(|| StoreError::InvalidMemory {
            message: "could not derive memory key; provide --key".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{derive_key, resolve_key};

    #[test]
    fn derives_stable_bounded_keys() {
        assert_eq!(
            derive_key("Always run Rust tests before closing"),
            Some("always-run-rust-tests-before-closing".to_owned())
        );
        assert!(derive_key("!!!").is_none());
    }

    #[test]
    fn explicit_key_and_invalid_content_are_distinct() {
        assert_eq!(
            resolve_key(Some("exact key".to_owned()), "content").expect("explicit key"),
            "exact key"
        );
        assert!(resolve_key(None, "   ").is_err());
        assert!(resolve_key(None, "!!!").is_err());
    }
}
