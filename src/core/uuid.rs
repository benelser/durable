//! UUID v4 generation — cross-platform, zero dependencies.
//!
//! Uses platform-native entropy:
//! - Unix: `/dev/urandom`
//! - Windows: `BCryptGenRandom` (via FFI)
//! - Fallback: high-entropy mixing of time + process + thread + stack addresses

use std::fmt;

/// A UUID v4 (random) identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// Generate a new random UUID v4.
    ///
    /// Cross-platform: uses `/dev/urandom` on Unix, `BCryptGenRandom` on
    /// Windows, and a high-entropy fallback on other platforms.
    pub fn new_v4() -> Self {
        let mut bytes = [0u8; 16];
        fill_random(&mut bytes);
        // Set version (4) and variant (RFC 4122)
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Uuid(bytes)
    }

    /// Create from raw bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Uuid(bytes)
    }

    /// Create a UUID from a hyphenated string like "550e8400-e29b-41d4-a716-446655440000".
    pub fn parse(s: &str) -> Result<Self, String> {
        let hex: String = s.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 {
            return Err(format!("invalid UUID length: {}", s));
        }
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| format!("invalid hex in UUID: {}", s))?;
        }
        Ok(Uuid(bytes))
    }

    /// Return the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Format as hyphenated lowercase string.
    pub fn to_hyphenated(&self) -> String {
        format!("{}", self)
    }
}

// ---------------------------------------------------------------------------
// Cross-platform random byte generation
// ---------------------------------------------------------------------------

/// Fill a buffer with cryptographically random bytes.
fn fill_random(buf: &mut [u8]) {
    if !platform_random(buf) {
        fallback_random(buf);
    }
}

/// Platform-native random bytes. Returns false if unavailable.
#[cfg(unix)]
fn platform_random(buf: &mut [u8]) -> bool {
    use std::fs::File;
    use std::io::Read;
    if let Ok(mut f) = File::open("/dev/urandom") {
        f.read_exact(buf).is_ok()
    } else {
        false
    }
}

/// Windows: use BCryptGenRandom via FFI (available since Vista).
#[cfg(windows)]
fn platform_random(buf: &mut [u8]) -> bool {
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            algorithm: *mut std::ffi::c_void,
            buffer: *mut u8,
            count: u32,
            flags: u32,
        ) -> i32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    status == 0 // STATUS_SUCCESS
}

/// Other platforms: no native RNG available.
#[cfg(not(any(unix, windows)))]
fn platform_random(_buf: &mut [u8]) -> bool {
    false
}

/// High-entropy fallback: mix multiple sources for collision resistance.
/// Not cryptographically secure but sufficient for UUID uniqueness.
fn fallback_random(buf: &mut [u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // Mix: time (nanoseconds) + process ID + thread ID + counter + stack address
    let time_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let pid = std::process::id() as u64;

    let thread_id = {
        let id = format!("{:?}", std::thread::current().id());
        let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
        for b in id.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3); // FNV prime
        }
        h
    };

    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);

    // Stack address as entropy (ASLR makes this unpredictable)
    let stack_addr = &counter as *const _ as u64;

    // SplitMix64 mixing function (better than XorShift for seeding)
    let mut state = time_ns
        .wrapping_add(pid.wrapping_mul(0x9e3779b97f4a7c15))
        .wrapping_add(thread_id)
        .wrapping_add(counter.wrapping_mul(0x6a09e667f3bcc908))
        .wrapping_add(stack_addr);

    for chunk in buf.chunks_mut(8) {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z = z ^ (z >> 31);
        let bytes = z.to_le_bytes();
        for (i, byte) in chunk.iter_mut().enumerate() {
            if i < 8 {
                *byte = bytes[i];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Display, Debug, JSON
// ---------------------------------------------------------------------------

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({})", self)
    }
}

impl crate::json::ToJson for Uuid {
    fn to_json(&self) -> crate::json::Value {
        crate::json::Value::String(self.to_hyphenated())
    }
}

impl crate::json::FromJson for Uuid {
    fn from_json(val: &crate::json::Value) -> Result<Self, String> {
        let s = val.as_str().ok_or("expected string for UUID")?;
        Uuid::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_roundtrip() {
        let id = Uuid::new_v4();
        let s = id.to_hyphenated();
        let parsed = Uuid::parse(&s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_uuid_uniqueness() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_ne!(a, b);
    }

    #[test]
    fn test_uuid_version_bits() {
        let id = Uuid::new_v4();
        assert_eq!(id.0[6] >> 4, 4); // version 4
        assert_eq!(id.0[8] >> 6, 2); // variant 10
    }

    #[test]
    fn test_uuid_from_bytes() {
        let bytes = [1u8; 16];
        let id = Uuid::from_bytes(bytes);
        assert_eq!(id.as_bytes(), &bytes);
    }

    #[test]
    fn test_uuid_bulk_uniqueness() {
        // Generate 1000 UUIDs and verify no collisions
        let mut set = std::collections::HashSet::new();
        for _ in 0..1000 {
            let id = Uuid::new_v4();
            assert!(set.insert(id), "UUID collision detected");
        }
    }
}
