//! Calendar arithmetic without a date dependency.
//!
//! This tool runs as an offline one-shot on the mail host, so it keeps its
//! dependency surface to crates already present in the workspace lockfile.
//! Everything here is pure and unit-tested. Certificate timestamps come from
//! 'openssl x509 -startdate/-enddate' in their long-stable ASN.1 form
//! ('Nov  1 23:19:45 2026 GMT'), not from the opt-in '-dateopt'.

use anyhow::{Result, bail};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds since the Unix epoch, or 0 if the host clock is before it.
pub fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
/// Parse the ASN.1 time 'openssl x509 -startdate' prints: 'Nov  1 23:19:45 2026 GMT'.
pub fn parse_asn1_time(value: &str) -> Result<i64> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 5 || parts[4] != "GMT" {
        bail!("unrecognized certificate time");
    }
    let month = MONTHS
        .iter()
        .position(|candidate| *candidate == parts[0])
        .map(|index| index as u32 + 1)
        .ok_or_else(|| anyhow::anyhow!("unrecognized certificate month"))?;
    let day: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("unrecognized certificate day"))?;
    let year: i64 = parts[3]
        .parse()
        .map_err(|_| anyhow::anyhow!("unrecognized certificate year"))?;
    let clock: Vec<&str> = parts[2].split(':').collect();
    if clock.len() != 3 {
        bail!("unrecognized certificate clock");
    }
    let hour: i64 = clock[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("unrecognized certificate clock"))?;
    let minute: i64 = clock[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("unrecognized certificate clock"))?;
    let second: i64 = clock[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("unrecognized certificate clock"))?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
        || !(1970..=9999).contains(&year)
    {
        bail!("certificate time is out of range");
    }
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// 'YYYY-MM-DDTHH:MM:SSZ'. The one format this tool writes into meta.json.
pub fn format_iso8601_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
    )
}

/// Inverse of format_iso8601_utc, used to read meta.json back.
pub fn parse_iso8601_utc(value: &str) -> Result<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20 || bytes[19] != b'Z' || bytes[4] != b'-' || bytes[7] != b'-' {
        bail!("unrecognized export timestamp");
    }
    if bytes[10] != b'T' && bytes[10] != b' ' {
        bail!("unrecognized export timestamp");
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        bail!("unrecognized export timestamp");
    }
    let number = |range: std::ops::Range<usize>| -> Result<i64> {
        std::str::from_utf8(&bytes[range])?
            .parse::<i64>()
            .map_err(|_| anyhow::anyhow!("unrecognized export timestamp"))
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
        || !(1970..=9999).contains(&year)
    {
        bail!("export timestamp is out of range");
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Ok(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month as i64 + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of days_from_civil.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asn1_times_round_trip_through_the_iso_format() {
        for (text, iso) in [
            ("Nov  1 23:19:45 2026 GMT", "2026-11-01T23:19:45Z"),
            ("Jan 30 00:00:00 2027 GMT", "2027-01-30T00:00:00Z"),
            ("Feb 29 12:00:00 2028 GMT", "2028-02-29T12:00:00Z"),
            ("Dec 31 23:59:59 1999 GMT", "1999-12-31T23:59:59Z"),
        ] {
            let epoch = parse_asn1_time(text).unwrap();
            assert_eq!(format_iso8601_utc(epoch), iso, "{text}");
            assert_eq!(parse_iso8601_utc(iso).unwrap(), epoch);
        }
    }

    #[test]
    fn epoch_zero_is_the_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn day_arithmetic_matches_known_offsets() {
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 29),
            1
        );
        for day in [-1, 0, 1, 29, 365, 10_957, 20_000] {
            let (year, month, day_of_month) = civil_from_days(day);
            assert_eq!(days_from_civil(year, month, day_of_month), day);
        }
    }

    #[test]
    fn malformed_times_are_refused_without_echoing_the_input() {
        for text in [
            "Nov  1 23:19:45 2026",
            "Foo  1 23:19:45 2026 GMT",
            "Nov 32 23:19:45 2026 GMT",
            "Nov  1 24:19:45 2026 GMT",
            "Nov  1 23:19 2026 GMT",
            "",
        ] {
            assert!(parse_asn1_time(text).is_err(), "{text}");
        }
        for text in [
            "2026-11-01T23:19:45",
            "2026-11-01T23:19:45.5Z",
            "x",
            "2026-13-01T00:00:00Z",
        ] {
            assert!(parse_iso8601_utc(text).is_err(), "{text}");
        }
    }
}
