//! Minimal UTC timestamp helpers.
//!
//! The update layer only needs RFC 3339 seconds precision. Keeping the
//! conversion local avoids another dependency for a handful of arithmetic
//! operations and keeps the output stable across platforms.

/// Convert a Unix timestamp (seconds) to `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (a trailing fractional part and `+00:00`
/// offset are tolerated). Returns Unix seconds.
pub fn parse_rfc3339_utc(text: &str) -> Option<u64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b't') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i64 = parse_digits(&bytes[0..4])?;
    let month: i64 = parse_digits(&bytes[5..7])?;
    let day: i64 = parse_digits(&bytes[8..10])?;
    let hour: i64 = parse_digits(&bytes[11..13])?;
    let minute: i64 = parse_digits(&bytes[14..16])?;
    let second: i64 = parse_digits(&bytes[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let mut rest = &bytes[19..];
    if rest.first() == Some(&b'.') {
        let digits = rest[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        rest = &rest[1 + digits..];
    }
    match rest {
        b"Z" | b"z" => {}
        b"+00:00" | b"-00:00" => {}
        _ => return None,
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

fn parse_digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0i64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

// Howard Hinnant's civil calendar algorithms.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_timestamps() {
        for seconds in [0, 1, 86_399, 1_700_000_000, 1_767_225_600] {
            let text = format_rfc3339(seconds);
            assert_eq!(parse_rfc3339_utc(&text), Some(seconds), "{text}");
        }
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rejects_bad_timestamps() {
        assert_eq!(parse_rfc3339_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_utc("2026-01-01 00:00:00Z"), None);
        assert_eq!(parse_rfc3339_utc(""), None);
    }
}
