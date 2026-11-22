//! Content hashing for change detection and reconciliation.

use sha2::{Digest, Sha256};

/// A content hash for a file, computed as SHA-256.
///
/// Used to detect actual content changes and reconcile missed events.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ContentHash {
    /// Hexadecimal representation of the SHA-256 hash.
    hex: String,
}

impl ContentHash {
    /// Compute the SHA-256 hash of some bytes.
    pub fn from_bytes(data: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        let hex = hex::encode(result);
        Self { hex }
    }

    /// Compute the SHA-256 hash of a string.
    pub fn from_str(data: &str) -> Self {
        Self::from_bytes(data.as_bytes())
    }

    /// Returns the hexadecimal representation.
    pub fn as_str(&self) -> &str {
        &self.hex
    }

    /// Creates a new empty hash (for files that haven't been indexed yet).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns true if this is an empty hash (uninitialized).
    pub fn is_empty(&self) -> bool {
        self.hex.is_empty()
    }
}

impl From<String> for ContentHash {
    fn from(hex: String) -> Self {
        Self { hex }
    }
}

impl AsRef<str> for ContentHash {
    fn as_ref(&self) -> &str {
        &self.hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable() {
        let data = b"test content";
        let h1 = ContentHash::from_bytes(data);
        let h2 = ContentHash::from_bytes(data);
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_content_different_hash() {
        let h1 = ContentHash::from_bytes(b"content 1");
        let h2 = ContentHash::from_bytes(b"content 2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_representation_is_hex() {
        let hash = ContentHash::from_bytes(b"hello");
        // SHA-256 produces 64 hex characters
        assert_eq!(hash.as_str().len(), 64);
        assert!(hash.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn empty_hash_is_distinct() {
        let empty = ContentHash::empty();
        assert!(empty.is_empty());
        let non_empty = ContentHash::from_bytes(b"");
        assert!(!non_empty.is_empty());
    }
}