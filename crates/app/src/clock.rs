//! Clock helpers: 12/24-hour formatting, date/time entry, and time zone grouping.

use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

/// Process-wide time format, set from config at startup and when changed in Settings.
static HOUR24: AtomicBool = AtomicBool::new(false);

pub fn set_hour24(on: bool) {
    HOUR24.store(on, Ordering::Relaxed);
}

pub fn hour24() -> bool {
    HOUR24.load(Ordering::Relaxed)
}

/// "8:05 PM" or "20:05" for a Unix timestamp, in local time.
pub fn format_time(t: i64, hour24: bool) -> String {
    let fmt = if hour24 { "%H:%M" } else { "%-I:%M %p" };
    Local.timestamp_opt(t, 0).single().map(|d| d.format(fmt).to_string()).unwrap_or_default()
}

/// Digits for "YYYY-MM-DD HH:MM".
pub const DATETIME_DIGITS: usize = 12;

/// Shows typed digits in the "____-__-__ __:__" template.
pub fn datetime_mask(digits: &str) -> String {
    let mut d = digits.chars();
    "____-__-__  __:__".chars().map(|c| if c == '_' { d.next().unwrap_or('_') } else { c }).collect()
}

/// Parses 12 digits "YYYYMMDDHHMM" (24-hour). Rejects impossible dates and years outside 2020–2099
/// (a TV box set to 1970 is a typo, not a choice).
pub fn parse_datetime(digits: &str) -> Option<NaiveDateTime> {
    if digits.len() != DATETIME_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| digits[r].parse::<u32>().ok();
    let year = n(0..4)? as i32;
    if !(2020..=2099).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year, n(4..6)?, n(6..8)?)?.and_hms_opt(n(8..10)?, n(10..12)?, 0)
}

/// Local wall-clock time → microseconds since the epoch. On a DST overlap, picks the earlier.
pub fn local_to_unix_usec(dt: NaiveDateTime) -> Option<i64> {
    Local.from_local_datetime(&dt).earliest().map(|d| d.timestamp() * 1_000_000)
}

/// Group for zones without a region ("UTC", "EST5EDT", ...).
pub const OTHER_REGION: &str = "Other";

/// Regions ("America", "Europe", ...), sorted, with `OTHER_REGION` last if present.
pub fn regions(zones: &[String]) -> Vec<String> {
    let mut r: Vec<String> = zones.iter().filter_map(|z| z.split_once('/').map(|(a, _)| a.to_string())).collect();
    r.sort();
    r.dedup();
    if zones.iter().any(|z| !z.contains('/')) {
        r.push(OTHER_REGION.into());
    }
    r
}

/// Zones in a region as (label, zone): "America/Argentina/Buenos_Aires" → "Argentina/Buenos Aires".
pub fn cities(zones: &[String], region: &str) -> Vec<(String, String)> {
    zones
        .iter()
        .filter_map(|z| {
            let label = match z.split_once('/') {
                Some((r, rest)) if r == region => rest,
                None if region == OTHER_REGION => z.as_str(),
                _ => return None,
            };
            Some((label.replace('_', " "), z.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_12_and_24_hour() {
        // Built from local wall time so the test doesn't depend on the machine's zone.
        let t = Local.with_ymd_and_hms(2026, 10, 22, 20, 5, 0).unwrap().timestamp();
        assert_eq!(format_time(t, false), "8:05 PM");
        assert_eq!(format_time(t, true), "20:05");
        let morning = Local.with_ymd_and_hms(2026, 10, 22, 7, 0, 0).unwrap().timestamp();
        assert_eq!(format_time(morning, true), "07:00");
    }

    #[test]
    fn parses_datetime_entry() {
        let dt = parse_datetime("202610222014").unwrap();
        assert_eq!(dt.to_string(), "2026-10-22 20:14:00");
        assert!(parse_datetime("202802291200").is_some()); // leap day
        assert!(parse_datetime("202702291200").is_none()); // not a leap year
        assert!(parse_datetime("202613011200").is_none()); // month 13
        assert!(parse_datetime("202610222460").is_none()); // 24:60
        assert!(parse_datetime("197001010000").is_none()); // out of range year
        assert!(parse_datetime("20261022201").is_none()); // too short
    }

    #[test]
    fn masks_partial_entry() {
        assert_eq!(datetime_mask(""), "____-__-__  __:__");
        assert_eq!(datetime_mask("202610"), "2026-10-__  __:__");
        assert_eq!(datetime_mask("202610222014"), "2026-10-22  20:14");
    }

    #[test]
    fn groups_zones_by_region() {
        let zones: Vec<String> = ["America/New_York", "America/Argentina/Buenos_Aires", "Europe/Paris", "UTC"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(regions(&zones), ["America", "Europe", "Other"]);
        assert_eq!(
            cities(&zones, "America"),
            [
                ("New York".to_string(), "America/New_York".to_string()),
                ("Argentina/Buenos Aires".to_string(), "America/Argentina/Buenos_Aires".to_string())
            ]
        );
        assert_eq!(cities(&zones, OTHER_REGION), [("UTC".to_string(), "UTC".to_string())]);
    }
}
