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

/// Hash tool definitions for drift detection.
/// Sorts by name for deterministic ordering, serializes each to JSON,
/// concatenates with separator, and hashes the result.
pub fn hash_tool_definitions(definitions: &[crate::tool::ToolDefinition]) -> u64 {
    let mut sorted: Vec<&crate::tool::ToolDefinition> = definitions.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut combined = String::new();
    for (i, def) in sorted.iter().enumerate() {
        if i > 0 {
            combined.push('|');
        }
        combined.push_str(&def.name);
        combined.push(':');
        combined.push_str(&def.description);
        combined.push(':');
        combined.push_str(&crate::json::to_string(&def.parameters));
    }
    fnv1a_hash(combined.as_bytes())
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
