//! ISO 8601 UTC timestamp helpers — no external crate dependency.
//!
//! Centralizes the Gregorian date math used by Slack (`<unix>.<usec>` ts strings)
//! and Gateway (`SystemTime::now()`) so both adapters share one implementation.

use std::time::{SystemTime, UNIX_EPOCH};

/// Convert days since the Unix epoch (1970-01-01) to a Gregorian (year, month, day).
/// Algorithm from <https://howardhinnant.github.io/date_algorithms.html>.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format a Unix timestamp (seconds + millis) as ISO 8601 UTC with millisecond precision.
fn unix_to_iso8601(secs: u64, ms: u64) -> String {
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let h = time_secs / 3600;
    let m = (time_secs % 3600) / 60;
    let s = time_secs % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

/// Convert a Slack `ts` string ("<unix_seconds>.<microseconds>") to ISO 8601 UTC.
/// Best-effort; falls back to epoch on parse failure.
///
/// Parses as `f64` so the fractional part carries decimal semantics directly —
/// ".12" maps to 120 ms, not 12 ms — without any string-padding gymnastics.
pub fn slack_ts_to_iso8601(ts: &str) -> String {
    let total = ts.parse::<f64>().unwrap_or(0.0);
    let secs = total.trunc() as u64;
    let ms = (total.fract() * 1000.0).round() as u64;
    unix_to_iso8601(secs, ms)
}

/// Current wall-clock instant as ISO 8601 UTC with millisecond precision.
pub fn now_iso8601() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    unix_to_iso8601(dur.as_secs(), (dur.subsec_millis()) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_ts_epoch_zero() {
        assert_eq!(slack_ts_to_iso8601("0.000000"), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn slack_ts_keeps_milliseconds() {
        // 1714204397 = 2024-04-27T07:53:17 UTC; .123456 → .123 ms
        assert_eq!(slack_ts_to_iso8601("1714204397.123456"), "2024-04-27T07:53:17.123Z");
    }

    #[test]
    fn slack_ts_missing_fraction_uses_zero() {
        assert_eq!(slack_ts_to_iso8601("1714204397"), "2024-04-27T07:53:17.000Z");
    }

    #[test]
    fn slack_ts_two_digit_fraction_is_120ms_not_12ms() {
        // ".12" carries decimal semantics: 0.12 s = 120 ms.
        assert_eq!(slack_ts_to_iso8601("1714204397.12"), "2024-04-27T07:53:17.120Z");
    }

    #[test]
    fn slack_ts_one_digit_fraction_is_100ms_not_1ms() {
        // ".1" carries decimal semantics: 0.1 s = 100 ms.
        assert_eq!(slack_ts_to_iso8601("1714204397.1"), "2024-04-27T07:53:17.100Z");
    }

    #[test]
    fn slack_ts_unparseable_falls_back_to_epoch() {
        assert_eq!(slack_ts_to_iso8601("not-a-ts"), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn now_iso8601_has_expected_shape() {
        let s = now_iso8601();
        // YYYY-MM-DDTHH:MM:SS.mmmZ = 24 chars
        assert_eq!(s.len(), 24);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..20], ".");
    }
}
