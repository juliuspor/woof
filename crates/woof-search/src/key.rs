use sha2::{Digest, Sha256};

/// Version tag for the deterministic vector-key mapping.
pub const KEY_DERIVATION_VERSION: &str = "woof.vector-key.sha256-be-v1";
const DOMAIN: &[u8] = b"woof.vector-key.sha256-be-v1\0";

/// Derives a stable nonzero u64 key from a record namespace and stable ID.
///
/// Algorithm: SHA-256 over the fixed domain, UTF-8 namespace, a NUL separator,
/// and UTF-8 stable ID; interpret the first eight digest bytes as big-endian.
/// Digest value zero is mapped to one because zero is commonly reserved by
/// native indexes. Rebuild rejects the astronomically unlikely collision.
pub fn derive_vector_key(namespace: &str, stable_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(stable_id.as_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic_and_domain_separated() {
        let key = derive_vector_key("snapshot", "42");
        assert_eq!(key, 767_850_511_492_278_297);
        assert_eq!(key, derive_vector_key("snapshot", "42"));
        assert_ne!(key, derive_vector_key("wiki", "42"));
        assert_ne!(key, derive_vector_key("snapshot", "43"));
        assert_ne!(key, 0);
    }
}
