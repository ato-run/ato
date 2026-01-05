use thiserror::Error;

/// Memory parsing error.
#[derive(Error, Debug, PartialEq)]
pub enum ParseError {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
    #[error("Unknown unit: {0}")]
    UnknownUnit(String),
    #[error("Parse float error: {0}")]
    ParseFloatError(#[from] std::num::ParseFloatError),
}

/// Parse a memory quantity string into bytes.
///
/// Examples:
/// - "4GB" -> 4294967296
/// - "512MB" -> 536870912
/// - "0GB" -> 0
/// - "1.5GB" -> 1610612736
pub fn parse_memory_string(s: &str) -> Result<u64, ParseError> {
    let s = s.trim().to_uppercase();
    if s == "0" {
        return Ok(0);
    }

    // find first non-digit / non-dot character
    let split_idx = s
        .find(|c: char| !c.is_numeric() && c != '.')
        .unwrap_or(s.len());
    let (num_str, unit) = s.split_at(split_idx);

    if num_str.is_empty() {
        return Err(ParseError::InvalidFormat(s));
    }

    let num: f64 = num_str.parse()?;

    let multiplier = match unit.trim() {
        "TB" | "T" => 1024.0_f64.powf(4.0),
        "GB" | "G" => 1024.0_f64.powf(3.0),
        "MB" | "M" => 1024.0_f64.powf(2.0),
        "KB" | "K" => 1024.0_f64,
        "B" | "" => 1.0,
        _ => return Err(ParseError::UnknownUnit(unit.to_string())),
    };

    Ok((num * multiplier) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory() {
        assert_eq!(parse_memory_string("4GB").unwrap(), 4 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory_string("512MB").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory_string("0GB").unwrap(), 0);
        assert_eq!(
            parse_memory_string("1.5GB").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert!(parse_memory_string("invalid").is_err());
    }
}
