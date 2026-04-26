use sha2::{Digest, Sha256};

pub fn compute_blake3_label(data: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(data).to_hex())
}

pub fn compute_sha256_label(data: &[u8]) -> String {
    format!("sha256:{}", compute_sha256_hex(data))
}

pub fn compute_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn normalize_hash_for_compare(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("sha256:")
        .trim_start_matches("blake3:")
        .to_ascii_lowercase()
}

pub fn equals_hash(expected: &str, got: &str) -> bool {
    normalize_hash_for_compare(expected) == normalize_hash_for_compare(got)
}
