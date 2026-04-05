//! FNV-1a hash for content-addressed step caching.

/// FNV-1a 64-bit hash of a byte slice.
/// Fast, deterministic, good distribution for cache keys.
pub fn fnv1a_hash(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Hash a JSON-serialized value for parameter comparison.
/// The JSON string is deterministic (BTreeMap keys are sorted).
pub fn hash_params(json_str: &str) -> u64 {
    fnv1a_hash(json_str.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic() {
        let a = fnv1a_hash(b"hello world");
        let b = fnv1a_hash(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn test_different_inputs() {
        let a = fnv1a_hash(b"hello");
        let b = fnv1a_hash(b"world");
        assert_ne!(a, b);
    }
}
