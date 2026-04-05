//! Timestamp utilities using std::time.
//!
//! Two clocks:
//! - `now_millis()` — wall-clock time (for timestamps, event ordering)
//! - `monotonic_millis()` — monotonic time (for timeouts, timers)
//!
//! Wall-clock can go backward (NTP, admin change). Monotonic never does.

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Process start time for monotonic clock baseline.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn process_start() -> &'static Instant {
    PROCESS_START.get_or_init(Instant::now)
}

/// Wall-clock milliseconds since Unix epoch.
/// Can go backward on clock adjustment — use for timestamps only.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// Monotonic milliseconds since process start.
/// Never goes backward — use for timeouts and timer calculations.
pub fn monotonic_millis() -> u64 {
    Instant::now()
        .duration_since(*process_start())
        .as_millis() as u64
}

/// ISO 8601 formatted timestamp (UTC, simplified).
pub fn now_iso8601() -> String {
    let millis = now_millis();
    let secs = millis / 1000;
    let ms = millis % 1000;

    // Simple UTC time calculation
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Date from days since epoch (simplified Gregorian)
    let (year, month, day) = days_to_ymd(days as i64);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hours, minutes, seconds, ms
    )
}

fn days_to_ymd(mut days: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = (days - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Parse a duration from a human-friendly string like "5s", "100ms", "2m".
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.ends_with("ms") {
        let n: u64 = s[..s.len() - 2]
            .parse()
            .map_err(|_| format!("invalid duration: {}", s))?;
        Ok(Duration::from_millis(n))
    } else if s.ends_with('s') {
        let n: u64 = s[..s.len() - 1]
            .parse()
            .map_err(|_| format!("invalid duration: {}", s))?;
        Ok(Duration::from_secs(n))
    } else if s.ends_with('m') {
        let n: u64 = s[..s.len() - 1]
            .parse()
            .map_err(|_| format!("invalid duration: {}", s))?;
        Ok(Duration::from_secs(n * 60))
    } else if s.ends_with('h') {
        let n: u64 = s[..s.len() - 1]
            .parse()
            .map_err(|_| format!("invalid duration: {}", s))?;
        Ok(Duration::from_secs(n * 3600))
    } else {
        Err(format!("invalid duration format: {}", s))
    }
}
