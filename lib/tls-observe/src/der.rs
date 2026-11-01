//! Minimal, bounds-checked DER reading for the certificate facts the probe
//! reports: serial number, validity, and subjectAltName dNSName entries.
//!
//! This is deliberately not a general X.509 parser. It walks only the fixed
//! skeleton of Certificate ::= SEQUENCE { tbsCertificate, ... } and the one
//! extension the probe needs. Every read is bounds-checked and every malformed
//! input is an Err; nothing here panics on hostile bytes.

/// The certificate facts extracted from the leaf DER.
pub(crate) struct LeafFacts {
    pub(crate) serial_hex: String,
    pub(crate) san: Vec<String>,
    pub(crate) not_before_epoch: i64,
    pub(crate) not_after_epoch: i64,
}

pub(crate) const SECONDS_PER_DAY: i64 = 86_400;

const TAG_BOOLEAN: u8 = 0x01;
const TAG_INTEGER: u8 = 0x02;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_OID: u8 = 0x06;
const TAG_UTC_TIME: u8 = 0x17;
const TAG_GENERALIZED_TIME: u8 = 0x18;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_CONTEXT_0: u8 = 0xa0;
const TAG_CONTEXT_3: u8 = 0xa3;
const TAG_DNS_NAME: u8 = 0x82;

/// OID 2.5.29.17 (subjectAltName), DER-encoded.
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];

/// A cursor over a DER byte string.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| "DER length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("truncated DER value".to_string());
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn read_byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    /// Read one tag-length-value triple, returning the tag and its content.
    fn read_tlv(&mut self) -> Result<(u8, &'a [u8]), String> {
        let tag = self.read_byte()?;
        if tag & 0x1f == 0x1f {
            // High-tag-number form. No fact we read uses it, so skip the
            // continuation bytes (bounded) and let the tag mismatch decide.
            let mut continued = 0u8;
            loop {
                let byte = self.read_byte()?;
                if byte & 0x80 == 0 {
                    break;
                }
                continued += 1;
                if continued > 4 {
                    return Err("DER tag number is too long".to_string());
                }
            }
        }
        let first = self.read_byte()?;
        let length = if first & 0x80 == 0 {
            usize::from(first)
        } else {
            let octets = usize::from(first & 0x7f);
            if octets == 0 {
                return Err("indefinite-length DER is not supported".to_string());
            }
            if octets > 4 {
                return Err("DER length is too large".to_string());
            }
            let mut length = 0usize;
            for _ in 0..octets {
                length = length
                    .checked_mul(256)
                    .ok_or_else(|| "DER length overflow".to_string())?;
                length = length
                    .checked_add(usize::from(self.read_byte()?))
                    .ok_or_else(|| "DER length overflow".to_string())?;
            }
            length
        };
        Ok((tag, self.take(length)?))
    }

    fn expect(&mut self, tag: u8) -> Result<&'a [u8], String> {
        let (actual, value) = self.read_tlv()?;
        if actual != tag {
            return Err(format!(
                "expected DER tag 0x{tag:02x} but found 0x{actual:02x}"
            ));
        }
        Ok(value)
    }
}

/// Extract the serial number, validity, and dNSName SANs from a certificate.
pub(crate) fn parse_certificate(der: &[u8]) -> Result<LeafFacts, String> {
    let mut outer = Reader::new(der);
    let certificate = outer.expect(TAG_SEQUENCE)?;
    let mut certificate = Reader::new(certificate);

    let tbs_certificate = certificate.expect(TAG_SEQUENCE)?;
    let mut tbs = Reader::new(tbs_certificate);

    if tbs.peek_tag() == Some(TAG_CONTEXT_0) {
        // version [0] EXPLICIT Version DEFAULT v1
        tbs.read_tlv()?;
    }

    let serial = tbs.expect(TAG_INTEGER)?;
    let serial_hex = hex_lower(serial);

    // signature AlgorithmIdentifier
    tbs.expect(TAG_SEQUENCE)?;
    // issuer Name
    tbs.expect(TAG_SEQUENCE)?;

    let validity = tbs.expect(TAG_SEQUENCE)?;
    let mut validity = Reader::new(validity);
    let (not_before_tag, not_before) = validity.read_tlv()?;
    let not_before_epoch = parse_time(not_before_tag, not_before)?;
    let (not_after_tag, not_after) = validity.read_tlv()?;
    let not_after_epoch = parse_time(not_after_tag, not_after)?;

    // subject Name
    tbs.expect(TAG_SEQUENCE)?;
    // subjectPublicKeyInfo
    tbs.expect(TAG_SEQUENCE)?;

    let mut san = Vec::new();
    while !tbs.is_empty() {
        let (tag, value) = tbs.read_tlv()?;
        // issuerUniqueID [1] and subjectUniqueID [2] carry no fact we report.
        if tag == TAG_CONTEXT_3 {
            // extensions [3] EXPLICIT Extensions
            san = parse_extensions(value)?;
        }
    }

    Ok(LeafFacts {
        serial_hex,
        san,
        not_before_epoch,
        not_after_epoch,
    })
}

/// Find the subjectAltName extension and return its dNSName entries.
fn parse_extensions(explicit_extensions: &[u8]) -> Result<Vec<String>, String> {
    let mut outer = Reader::new(explicit_extensions);
    let extensions = outer.expect(TAG_SEQUENCE)?;
    let mut reader = Reader::new(extensions);
    while !reader.is_empty() {
        let extension = reader.expect(TAG_SEQUENCE)?;
        let mut fields = Reader::new(extension);
        let oid = fields.expect(TAG_OID)?;
        if fields.peek_tag() == Some(TAG_BOOLEAN) {
            // critical BOOLEAN DEFAULT FALSE
            fields.read_tlv()?;
        }
        let value = fields.expect(TAG_OCTET_STRING)?;
        if oid == OID_SUBJECT_ALT_NAME {
            return parse_general_names(value);
        }
    }
    Ok(Vec::new())
}

fn parse_general_names(value: &[u8]) -> Result<Vec<String>, String> {
    let mut outer = Reader::new(value);
    let names = outer.expect(TAG_SEQUENCE)?;
    let mut reader = Reader::new(names);
    let mut san: Vec<String> = Vec::new();
    while !reader.is_empty() {
        let (tag, name) = reader.read_tlv()?;
        if tag == TAG_DNS_NAME
            && let Some(text) = dns_name_text(name)
            && !san.contains(&text)
        {
            san.push(text);
        }
    }
    Ok(san)
}

/// IA5String is ASCII; anything else is not a name we can report.
fn dns_name_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || !bytes.iter().all(|byte| byte.is_ascii()) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    Some(text.to_ascii_lowercase())
}

fn parse_time(tag: u8, bytes: &[u8]) -> Result<i64, String> {
    let (year, rest) = match tag {
        TAG_UTC_TIME => {
            if bytes.len() != 13 || bytes[12] != b'Z' {
                return Err("malformed UTCTime".to_string());
            }
            let two_digit_year = decimal(&bytes[0..2])?;
            let year = if two_digit_year >= 50 {
                1900 + i64::from(two_digit_year)
            } else {
                2000 + i64::from(two_digit_year)
            };
            (year, &bytes[2..12])
        }
        TAG_GENERALIZED_TIME => {
            if bytes.len() != 15 || bytes[14] != b'Z' {
                return Err("malformed GeneralizedTime".to_string());
            }
            (i64::from(decimal(&bytes[0..4])?), &bytes[4..14])
        }
        _ => return Err("unsupported certificate time encoding".to_string()),
    };

    let month = decimal(&rest[0..2])?;
    let day = decimal(&rest[2..4])?;
    let hour = decimal(&rest[4..6])?;
    let minute = decimal(&rest[6..8])?;
    let second = decimal(&rest[8..10])?;

    if !(1..=12).contains(&month) {
        return Err(format!("invalid certificate month {month}"));
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(format!("invalid certificate day {day}"));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err("invalid certificate time of day".to_string());
    }

    let days = days_from_civil(year, month, day);
    Ok(
        days * SECONDS_PER_DAY
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

fn decimal(bytes: &[u8]) -> Result<u32, String> {
    let mut value = 0u32;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return Err("non-digit in certificate time".to_string());
        }
        value = value * 10 + u32::from(byte - b'0');
    }
    Ok(value)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian civil date.
///
/// Howard Hinnant's days_from_civil; exact for every year we can parse.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    hex
}

/// Whether a presented SAN dNSName covers a server name.
///
/// RFC 6125: an exact, case-insensitive match, or a *.example.com wildcard
/// covering exactly one left-most label.
pub(crate) fn server_name_matches(san: &[String], server_name: &str) -> bool {
    let server_name = server_name.to_ascii_lowercase();
    if server_name.is_empty() {
        return false;
    }
    san.iter()
        .any(|pattern| san_pattern_matches(pattern, &server_name))
}

fn san_pattern_matches(pattern: &str, server_name: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => {
            if suffix.contains('*') || suffix.is_empty() {
                return false;
            }
            let suffix = format!(".{suffix}");
            server_name.len() > suffix.len()
                && server_name.ends_with(&suffix)
                && !server_name[..server_name.len() - suffix.len()].contains('.')
        }
        None => !pattern.contains('*') && pattern == server_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC_2026: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

    #[test]
    fn utc_time_is_parsed_in_the_1950_to_2049_window() {
        assert_eq!(parse_time(TAG_UTC_TIME, b"260101000000Z"), Ok(UTC_2026));
        assert_eq!(
            parse_time(TAG_UTC_TIME, b"500101000000Z"),
            Ok(-631_152_000) // 1950-01-01T00:00:00Z
        );
        assert_eq!(
            parse_time(TAG_UTC_TIME, b"491231235959Z"),
            Ok(2_524_607_999) // 2049-12-31T23:59:59Z
        );
    }

    #[test]
    fn generalized_time_is_parsed_with_a_four_digit_year() {
        assert_eq!(
            parse_time(TAG_GENERALIZED_TIME, b"20260101000000Z"),
            Ok(UTC_2026)
        );
        assert_eq!(
            parse_time(TAG_GENERALIZED_TIME, b"20460101000000Z"),
            Ok(UTC_2026 + 20 * 365 * SECONDS_PER_DAY + 5 * SECONDS_PER_DAY)
        );
    }

    #[test]
    fn leap_day_is_parsed() {
        assert_eq!(
            parse_time(TAG_GENERALIZED_TIME, b"20240229000000Z"),
            Ok(1_709_164_800) // 2024-02-29T00:00:00Z
        );
    }

    #[test]
    fn malformed_times_are_errors_not_panics() {
        for bytes in [
            b"".as_slice(),
            b"260101000000".as_slice(),
            b"260101000000+".as_slice(),
            b"261301000000Z".as_slice(),
            b"260132000000Z".as_slice(),
            b"260101240000Z".as_slice(),
            b"260101006000Z".as_slice(),
            b"26aa01000000Z".as_slice(),
            b"20460229000000Z".as_slice(),
        ] {
            assert!(parse_time(TAG_GENERALIZED_TIME, bytes).is_err());
        }
        assert!(parse_time(TAG_BOOLEAN, b"260101000000Z").is_err());
    }

    #[test]
    fn wildcard_covers_exactly_one_label() {
        let san = vec!["*.example.com".to_string()];
        assert!(server_name_matches(&san, "mail.example.com"));
        assert!(server_name_matches(&san, "MAIL.EXAMPLE.COM"));
        assert!(!server_name_matches(&san, "example.com"));
        assert!(!server_name_matches(&san, "a.b.example.com"));
        assert!(!server_name_matches(&san, ".example.com"));
    }

    #[test]
    fn exact_names_match_case_insensitively() {
        let san = vec!["mail.ato.test".to_string(), "mail.example".to_string()];
        assert!(server_name_matches(&san, "mail.ato.test"));
        assert!(server_name_matches(&san, "Mail.Ato.Test"));
        assert!(!server_name_matches(&san, "ato.test"));
        assert!(!server_name_matches(&san, "other.invalid"));
        assert!(!server_name_matches(&[], "mail.ato.test"));
    }

    #[test]
    fn wildcards_elsewhere_are_not_wildcards() {
        let san = vec!["mail.*.test".to_string(), "*".to_string()];
        assert!(!server_name_matches(&san, "mail.ato.test"));
        assert!(!server_name_matches(&san, "*"));
    }

    #[test]
    fn der_reader_rejects_truncated_and_oversized_lengths() {
        let mut reader = Reader::new(&[0x30, 0x05, 0x01]);
        assert!(reader.read_tlv().is_err());

        let mut reader = Reader::new(&[0x30, 0x89, 0x00]);
        assert!(reader.read_tlv().is_err());

        let well_formed = Reader::new(&[0x02, 0x01, 0x01]).read_tlv();
        assert!(well_formed.is_ok());
    }
}
