//! Wall-clock time for log lines and crash file names, in UTC.
//!
//! # Theory
//!
//! The log is read next to other machines' logs and an ingest's, so its
//! times are UTC with milliseconds, as RFC 3339 writes them
//! (`2026-10-28T09:41:05.123Z`); crash files are named by the second
//! (`20261028-094105`), which also sorts them in time. The calendar date
//! comes from the day count since 1970 by Howard Hinnant's
//! `civil_from_days`, exact for the proleptic Gregorian calendar, so no
//! date crate is needed.
//!
//! # Invariants
//!
//! * A time before 1970 (a clock set wrong) reads as the epoch: a log
//!   line always gets a well-formed time.
//! * Fields are zero-padded to fixed widths, so lines align and names
//!   sort.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A UTC calendar time, to the millisecond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utc {
    /// Year.
    pub year: i64,
    /// Month, `1..=12`.
    pub month: u32,
    /// Day of the month, `1..=31`.
    pub day: u32,
    /// Hour, `0..=23`.
    pub hour: u32,
    /// Minute, `0..=59`.
    pub minute: u32,
    /// Second, `0..=59`.
    pub second: u32,
    /// Millisecond, `0..=999`.
    pub millis: u32,
}

impl Utc {
    /// Now, from the system clock.
    pub fn now() -> Self {
        Self::at(SystemTime::now())
    }

    /// The calendar time of `t`; see the module docs for times before 1970.
    pub fn at(t: SystemTime) -> Self {
        Self::since_epoch(t.duration_since(UNIX_EPOCH).unwrap_or_default())
    }

    /// The calendar time `since` after 1970-01-01T00:00:00Z.
    pub fn since_epoch(since: Duration) -> Self {
        let secs = since.as_secs();
        let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
        let in_day = secs % 86_400;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (in_day / 3600) as u32,
            minute: (in_day / 60 % 60) as u32,
            second: (in_day % 60) as u32,
            millis: since.subsec_millis(),
        }
    }

    /// `2026-10-28T09:41:05.123Z`.
    pub fn rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }

    /// `20261028-094105`, for file names.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Year, month and day of day `z` counted from 1970-01-01 (Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 3339 text of `secs` and `millis` after the epoch.
    fn text(secs: u64, millis: u64) -> String {
        Utc::since_epoch(Duration::from_secs(secs) + Duration::from_millis(millis)).rfc3339()
    }

    #[test]
    fn known_instants() {
        assert_eq!(text(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(text(1_700_000_000, 42), "2023-11-14T22:13:20.042Z");
        assert_eq!(text(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        assert_eq!(text(1_709_251_199, 999), "2024-02-29T23:59:59.999Z");
        assert_eq!(text(1_709_251_200, 0), "2024-03-01T00:00:00.000Z");
        assert_eq!(text(4_102_444_800, 0), "2100-01-01T00:00:00.000Z");
        assert_eq!(text(1_798_761_599, 0), "2026-12-31T23:59:59.000Z");
    }

    #[test]
    fn file_names_are_compact_and_sort_in_time() {
        let a = Utc::since_epoch(Duration::from_secs(1_761_644_465)).compact();
        assert_eq!(a, "20251028-094105");
        let b = Utc::since_epoch(Duration::from_secs(1_761_644_466)).compact();
        assert!(a < b);
    }

    #[test]
    fn a_clock_before_1970_reads_as_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(Utc::at(before).rfc3339(), "1970-01-01T00:00:00.000Z");
        let now = Utc::now();
        assert!(now.year >= 2024 && (1..=12).contains(&now.month));
    }
}
