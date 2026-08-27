//! Deterministic time for the host.
//!
//! Every durable timestamp and every TTL decision goes through a [`Clock`], so
//! restart recovery, lease expiry, and escalation expiry are reproducible in
//! tests without sleeping or reading the wall clock.

use std::sync::atomic::{AtomicU64, Ordering};

/// Source of host time.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;

    /// The same instant as an RFC3339 UTC timestamp.
    fn now_rfc3339(&self) -> String {
        rfc3339_from_epoch_ms(self.now_ms())
    }
}

/// Wall-clock time. Used by the binary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or_default()
    }
}

/// Manually advanced clock. Used by fixtures and contract tests.
#[derive(Debug)]
pub struct FixedClock {
    now_ms: AtomicU64,
}

impl FixedClock {
    /// Start at an explicit epoch millisecond.
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    /// Move time forward by an exact number of milliseconds.
    pub fn advance(&self, delta_ms: u64) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// Format epoch milliseconds as an RFC3339 UTC timestamp.
pub fn rfc3339_from_epoch_ms(epoch_ms: u64) -> String {
    let total_seconds = (epoch_ms / 1_000) as i64;
    let millis = epoch_ms % 1_000;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a proleptic
/// Gregorian date. Chosen over a date dependency to keep the host's public
/// build free of extra crates.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = (if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    }) / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_instants_format_exactly() {
        assert_eq!(rfc3339_from_epoch_ms(0), "1970-01-01T00:00:00.000Z");
        // 2026-08-28T12:34:56.789Z
        assert_eq!(
            rfc3339_from_epoch_ms(1_787_920_496_789),
            "2026-08-28T12:34:56.789Z"
        );
        // Leap day.
        assert_eq!(
            rfc3339_from_epoch_ms(1_709_164_800_000),
            "2024-02-29T00:00:00.000Z"
        );
    }

    #[test]
    fn fixed_clock_only_moves_when_told_to() {
        let clock = FixedClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        assert_eq!(clock.now_rfc3339(), clock.now_rfc3339());
        clock.advance(2_500);
        assert_eq!(clock.now_ms(), 3_500);
    }
}
