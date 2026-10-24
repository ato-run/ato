//! Minimal RFC 3339 UTC timestamp parsing.
//!
//! The inventory schema only needs to compare declared instants (scan times,
//! receipt observations, waiver expiries), so this module accepts exactly the
//! profile the schema documents: `YYYY-MM-DDTHH:MM:SSZ` with an optional
//! fractional second. Offsets other than `Z` are rejected instead of being
//! normalized, so every ledger file has one reviewable canonical form. Leap
//! seconds are rejected; the schema records ordinary wall-clock instants only.
//!
//! No `chrono` dependency: the parser is small, deterministic, and covered by
//! tests for the boundaries the validator actually depends on.

use std::fmt;

/// An instant in UTC, comparable with [`Ord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub unix_seconds: i64,
    pub nanos: u32,
}

impl Timestamp {
    pub fn from_unix_seconds(unix_seconds: i64) -> Self {
        Self {
            unix_seconds,
            nanos: 0,
        }
    }
}

/// Why a declared timestamp was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampError {
    detail: String,
}

impl TimestampError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TimestampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for TimestampError {}

/// The current instant, from the system clock.
pub fn now() -> Timestamp {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => Timestamp {
            unix_seconds: duration.as_secs() as i64,
            nanos: duration.subsec_nanos(),
        },
        Err(_) => Timestamp::from_unix_seconds(0),
    }
}

/// Parse the schema's canonical UTC form: `YYYY-MM-DDTHH:MM:SSZ`, optionally
/// with `.` followed by 1-9 fractional-second digits. `T` and `Z` may be
/// upper- or lower-case, as RFC 3339 allows.
pub fn parse(value: &str) -> Result<Timestamp, TimestampError> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return Err(TimestampError::new(
            "expected YYYY-MM-DDTHH:MM:SSZ with optional fractional seconds",
        ));
    }
    expect(byte_at(bytes, 4)?, b'-', "date separator at offset 4")?;
    expect(byte_at(bytes, 7)?, b'-', "date separator at offset 7")?;
    expect_time_separator(byte_at(bytes, 10)?)?;
    expect(byte_at(bytes, 13)?, b':', "time separator at offset 13")?;
    expect(byte_at(bytes, 16)?, b':', "time separator at offset 16")?;

    let year = number(bytes, 0, 4)?;
    let month = number(bytes, 5, 2)?;
    let day = number(bytes, 8, 2)?;
    let hour = number(bytes, 11, 2)?;
    let minute = number(bytes, 14, 2)?;
    let second = number(bytes, 17, 2)?;

    let mut index = 19;
    let mut nanos = 0u32;
    if bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digit_count = index - start;
        if digit_count == 0 || digit_count > 9 {
            return Err(TimestampError::new(
                "fractional seconds must have 1-9 digits",
            ));
        }
        for position in 0..9 {
            let digit = if position < digit_count {
                u32::from(bytes[start + position] - b'0')
            } else {
                0
            };
            nanos = nanos * 10 + digit;
        }
    }
    if index >= bytes.len() {
        return Err(TimestampError::new("missing Z (UTC designator)"));
    }
    match bytes[index] {
        b'Z' | b'z' => index += 1,
        _ => {
            return Err(TimestampError::new(
                "expected Z (only UTC timestamps are accepted)",
            ));
        }
    }
    if index != bytes.len() {
        return Err(TimestampError::new("trailing characters after Z"));
    }

    if !(1..=12).contains(&month) {
        return Err(TimestampError::new(format!(
            "month {month:02} is not 01-12"
        )));
    }
    let last_day = days_in_month(year, month);
    if !(1..=last_day).contains(&day) {
        return Err(TimestampError::new(format!(
            "day {day:02} is not 01-{last_day:02} for {year:04}-{month:02}"
        )));
    }
    if hour > 23 {
        return Err(TimestampError::new(format!("hour {hour:02} is not 00-23")));
    }
    if minute > 59 {
        return Err(TimestampError::new(format!(
            "minute {minute:02} is not 00-59"
        )));
    }
    if second > 59 {
        return Err(TimestampError::new(format!(
            "second {second:02} is not 00-59 (leap seconds are not accepted)"
        )));
    }

    let days = days_from_civil(year, month, day);
    let unix_seconds =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Ok(Timestamp {
        unix_seconds,
        nanos,
    })
}

fn byte_at(bytes: &[u8], index: usize) -> Result<u8, TimestampError> {
    bytes
        .get(index)
        .copied()
        .ok_or_else(|| TimestampError::new("timestamp is too short"))
}

fn expect(actual: u8, expected: u8, what: &str) -> Result<(), TimestampError> {
    if actual == expected {
        Ok(())
    } else {
        Err(TimestampError::new(format!(
            "expected '{}' as {what}, found '{}'",
            expected as char, actual as char
        )))
    }
}

fn expect_time_separator(actual: u8) -> Result<(), TimestampError> {
    if actual == b'T' || actual == b't' {
        Ok(())
    } else {
        Err(TimestampError::new(
            "expected 'T' between the date and the time",
        ))
    }
}

fn number(bytes: &[u8], start: usize, length: usize) -> Result<u32, TimestampError> {
    let mut value = 0u32;
    for index in start..start + length {
        let byte = *bytes
            .get(index)
            .ok_or_else(|| TimestampError::new("timestamp is too short"))?;
        if !byte.is_ascii_digit() {
            return Err(TimestampError::new(format!(
                "expected a digit at offset {index}, found '{}'",
                byte as char
            )));
        }
        value = value * 10 + u32::from(byte - b'0');
    }
    Ok(value)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whole_seconds() {
        let timestamp = parse("2026-10-24T16:56:43Z").expect("valid timestamp");
        assert_eq!(timestamp.unix_seconds, 1_792_861_003);
        assert_eq!(timestamp.nanos, 0);
    }

    #[test]
    fn parses_fractional_seconds() {
        let timestamp = parse("2026-10-24T16:56:43.5Z").expect("valid timestamp");
        assert_eq!(timestamp.unix_seconds, 1_792_861_003);
        assert_eq!(timestamp.nanos, 500_000_000);

        let nine_digits = parse("2026-10-24T16:56:43.123456789Z").expect("valid timestamp");
        assert_eq!(nine_digits.nanos, 123_456_789);

        let one_digit = parse("2026-10-24T16:56:43.1Z").expect("valid timestamp");
        assert_eq!(one_digit.nanos, 100_000_000);
    }

    #[test]
    fn accepts_lowercase_designators() {
        let lower = parse("2026-10-24t16:56:43z").expect("valid timestamp");
        let upper = parse("2026-10-24T16:56:43Z").expect("valid timestamp");
        assert_eq!(lower, upper);
    }

    #[test]
    fn compares_instants() {
        let earlier = parse("2026-10-24T16:56:43Z").expect("valid");
        let later = parse("2026-10-24T16:56:43.000000001Z").expect("valid");
        assert!(earlier < later);
        assert!(later > earlier);
    }

    #[test]
    fn accepts_leap_day() {
        parse("2024-02-29T00:00:00Z").expect("2024 is a leap year");
        assert_eq!(
            parse("2024-02-29T00:00:00Z").expect("valid").unix_seconds,
            1_709_164_800
        );
    }

    #[test]
    fn rejects_calendar_errors() {
        assert!(parse("2023-02-29T00:00:00Z").is_err());
        assert!(parse("2026-13-01T00:00:00Z").is_err());
        assert!(parse("2026-00-01T00:00:00Z").is_err());
        assert!(parse("2026-04-31T00:00:00Z").is_err());
        assert!(parse("2026-10-00T00:00:00Z").is_err());
        assert!(parse("2026-10-24T24:00:00Z").is_err());
        assert!(parse("2026-10-24T16:60:00Z").is_err());
        assert!(parse("2026-10-24T16:56:60Z").is_err());
    }

    #[test]
    fn rejects_non_utc_or_malformed_forms() {
        assert!(parse("2026-10-24T16:56:43+00:00").is_err());
        assert!(parse("2026-10-24T16:56:43").is_err());
        assert!(parse("2026-10-24 16:56:43Z").is_err());
        assert!(parse("2026-10-24T16:56:43.Z").is_err());
        assert!(parse("2026-10-24T16:56:43.1234567890Z").is_err());
        assert!(parse("2026-10-24T16:56:43Z ").is_err());
        assert!(parse("2026-10-24T16:56:4Z").is_err());
        assert!(parse("").is_err());
        assert!(parse("not-a-timestamp").is_err());
    }
}
