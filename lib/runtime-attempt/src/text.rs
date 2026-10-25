//! Failure text as it leaves a Runtime: one bounded line.

/// The control plane caps a failure reason at 400 characters.
pub const FAILURE_REASON_LIMIT: usize = 400;

/// Single-line, bounded failure text. Newlines become spaces so one failure
/// stays one readable sentence, and the cut walks back to a character boundary.
pub fn bounded_reason(reason: &str) -> String {
    let single: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = single.trim();
    if trimmed.len() <= FAILURE_REASON_LIMIT {
        return trimmed.to_owned();
    }
    let mut end = FAILURE_REASON_LIMIT;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_owned()
}
