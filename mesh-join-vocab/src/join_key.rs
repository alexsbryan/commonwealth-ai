// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt;

/// A join key does not match the mesh invite format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinKeyError(String);

impl fmt::Display for JoinKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JoinKeyError {}

/// Hash a join key using BLAKE3. The raw key is never persisted — only the hash.
pub fn hash_join_key(key: &str) -> [u8; 32] {
    *blake3::hash(key.as_bytes()).as_bytes()
}

/// Parse and validate join key format (`cwth-XXXX-XXXX-XXXX` where X is hex).
pub fn validate_join_key_format(key: &str) -> Result<(), JoinKeyError> {
    let parts: Vec<&str> = key.split('-').collect();
    if parts.len() != 4 || parts[0] != "cwth" {
        return Err(JoinKeyError("expected format cwth-XXXX-XXXX-XXXX".into()));
    }
    for part in &parts[1..] {
        if part.len() != 4 || hex::decode(part).is_err() {
            return Err(JoinKeyError("each segment must be 4 hex characters".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{hash_join_key, validate_join_key_format};

    #[test]
    fn join_key_format() {
        validate_join_key_format("cwth-7f3a-9b2e-4d1c").unwrap();
    }

    #[test]
    fn hash_join_key_returns_the_blake3_digest() {
        let key = "cwth-7f3a-9b2e-4d1c";
        assert_eq!(hash_join_key(key), *blake3::hash(key.as_bytes()).as_bytes());
    }

    #[test]
    fn validate_join_key_format_rejects_bad_keys() {
        assert!(validate_join_key_format("not-a-key").is_err());
        assert!(validate_join_key_format("cwth-zzzz-0000-0000").is_err());
        assert!(validate_join_key_format("cwth-00-0000-0000").is_err());
        assert!(validate_join_key_format("").is_err());
    }

    #[test]
    fn validate_join_key_format_preserves_error_messages() {
        assert_eq!(
            validate_join_key_format("not-a-key")
                .unwrap_err()
                .to_string(),
            "expected format cwth-XXXX-XXXX-XXXX"
        );
        assert_eq!(
            validate_join_key_format("cwth-zzzz-0000-0000")
                .unwrap_err()
                .to_string(),
            "each segment must be 4 hex characters"
        );
    }
}
