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

#[cfg(test)]
mod tests {
    use super::{
        compute_blake3_label, compute_sha256_hex, compute_sha256_label, equals_hash,
        normalize_hash_for_compare,
    };

    #[test]
    fn compute_blake3_label_prefixes_hash() {
        let hash = compute_blake3_label(b"hello world");
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash.len(), 7 + 64);
    }

    #[test]
    fn compute_sha256_hex_returns_unlabeled_hex() {
        let hash = compute_sha256_hex(b"hello world");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn compute_sha256_label_prefixes_hash() {
        let hash = compute_sha256_label(b"hello world");
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 7 + 64);
    }

    #[test]
    fn equals_hash_ignores_label_and_case() {
        let value = "b94d27b9934d3e08a52e52d7da7dabfade4f3e9e64c94f4db5d4ef7d6df4f6f6";
        assert!(equals_hash(value, value));
        assert!(equals_hash(&format!("sha256:{}", value), value));
        assert!(equals_hash(
            &format!("blake3:{}", value.to_ascii_uppercase()),
            value
        ));
    }

    #[test]
    fn normalize_hash_for_compare_trims_and_strips_prefix() {
        assert_eq!(normalize_hash_for_compare(" ABCDEF "), "abcdef");
        assert_eq!(normalize_hash_for_compare("sha256:ABCDEF"), "abcdef");
        assert_eq!(normalize_hash_for_compare("blake3:ABCDEF"), "abcdef");
    }
}
