use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

pub const DEFAULT_BLOOM_FALSE_POSITIVE_RATE: f64 = 0.01;
pub const DEFAULT_BLOOM_SEED: u64 = 0x4154_4f5f_5633_0001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtoBloomFilter {
    bits: Vec<u8>,
    k_hashes: u32,
    m_bits: u64,
    seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AtoBloomWire {
    pub m_bits: u64,
    pub k_hashes: u32,
    pub seed: u64,
    pub bitset_base64: String,
}

impl AtoBloomFilter {
    pub fn empty_with_seed(seed: u64) -> Self {
        Self {
            bits: vec![0u8; 1],
            k_hashes: 1,
            m_bits: 8,
            seed,
        }
    }

    pub fn from_hashes<I, S>(hashes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::from_hashes_with_params(
            hashes,
            DEFAULT_BLOOM_FALSE_POSITIVE_RATE,
            DEFAULT_BLOOM_SEED,
        )
    }

    pub fn from_hashes_with_params<I, S>(hashes: I, fp_rate: f64, seed: u64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let values: Vec<String> = hashes.into_iter().map(|v| v.as_ref().to_string()).collect();
        let n = values.len();
        if n == 0 {
            return Self::empty_with_seed(seed);
        }

        let p = if fp_rate > 0.0 && fp_rate < 1.0 {
            fp_rate
        } else {
            DEFAULT_BLOOM_FALSE_POSITIVE_RATE
        };
        let ln2 = std::f64::consts::LN_2;
        let m_estimate = (-(n as f64) * p.ln() / (ln2 * ln2)).ceil();
        let m_bits = m_estimate.max(8.0) as usize;
        let k_estimate = ((m_bits as f64 / n as f64) * ln2).round() as i64;
        let k_hashes = k_estimate.max(1) as u32;

        let mut filter = Self {
            bits: vec![0u8; m_bits.div_ceil(8)],
            k_hashes,
            m_bits: m_bits as u64,
            seed,
        };
        for value in &values {
            filter.insert(value);
        }
        filter
    }

    pub fn k_hashes(&self) -> u32 {
        self.k_hashes
    }

    pub fn m_bits(&self) -> u64 {
        self.m_bits
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn bits(&self) -> &[u8] {
        &self.bits
    }

    pub fn insert(&mut self, value: &str) {
        for round in 0..self.k_hashes {
            let bit_index = self.bit_index(value, round);
            let byte_index = bit_index / 8;
            let bit_mask = 1u8 << (bit_index % 8);
            if let Some(byte) = self.bits.get_mut(byte_index) {
                *byte |= bit_mask;
            }
        }
    }

    pub fn might_contain(&self, value: &str) -> bool {
        (0..self.k_hashes).all(|round| {
            let bit_index = self.bit_index(value, round);
            let byte_index = bit_index / 8;
            let bit_mask = 1u8 << (bit_index % 8);
            self.bits
                .get(byte_index)
                .map(|byte| (*byte & bit_mask) != 0)
                .unwrap_or(false)
        })
    }

    pub fn to_wire(&self) -> AtoBloomWire {
        AtoBloomWire {
            m_bits: self.m_bits,
            k_hashes: self.k_hashes,
            seed: self.seed,
            bitset_base64: BASE64.encode(&self.bits),
        }
    }

    pub fn from_wire(wire: &AtoBloomWire) -> crate::error::Result<Self> {
        let bits = BASE64
            .decode(wire.bitset_base64.as_bytes())
            .map_err(|e| crate::error::CapsuleError::Runtime(format!("bloom base64 decode: {e}")))?;
        if bits.is_empty() {
            return Err(crate::error::CapsuleError::Runtime(
                "bloom bitset must not be empty".into(),
            ));
        }
        if wire.k_hashes == 0 {
            return Err(crate::error::CapsuleError::Runtime(
                "bloom k_hashes must be greater than zero".into(),
            ));
        }
        let available_bits = (bits.len() * 8) as u64;
        let m_bits = wire.m_bits.max(1).min(available_bits);
        Ok(Self {
            bits,
            k_hashes: wire.k_hashes,
            m_bits,
            seed: wire.seed,
        })
    }

    fn bit_index(&self, value: &str, round: u32) -> usize {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.seed.to_le_bytes());
        hasher.update(&round.to_le_bytes());
        hasher.update(value.as_bytes());
        let digest = hasher.finalize();
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&digest.as_bytes()[..8]);
        (u64::from_le_bytes(raw) % self.m_bits) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::{AtoBloomFilter, DEFAULT_BLOOM_SEED};

    #[test]
    fn deterministic_for_same_set() {
        let first = AtoBloomFilter::from_hashes_with_params(
            ["blake3:aaa", "blake3:bbb", "blake3:ccc"],
            0.01,
            DEFAULT_BLOOM_SEED,
        );
        let second = AtoBloomFilter::from_hashes_with_params(
            ["blake3:ccc", "blake3:aaa", "blake3:bbb"],
            0.01,
            DEFAULT_BLOOM_SEED,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn m_k_are_stable_for_empty_input() {
        let filter = AtoBloomFilter::from_hashes::<Vec<&str>, &str>(vec![]);
        assert_eq!(filter.m_bits(), 8);
        assert_eq!(filter.k_hashes(), 1);
        assert_eq!(filter.bits().len(), 1);
    }

    #[test]
    fn inserted_value_is_always_present() {
        let mut filter = AtoBloomFilter::empty_with_seed(DEFAULT_BLOOM_SEED);
        filter.insert("blake3:deadbeef");
        assert!(filter.might_contain("blake3:deadbeef"));
    }

    #[test]
    fn wire_roundtrip_preserves_bits() {
        let filter = AtoBloomFilter::from_hashes(["blake3:111", "blake3:222", "blake3:333"]);
        let wire = filter.to_wire();
        let decoded = AtoBloomFilter::from_wire(&wire).expect("decode");
        assert_eq!(decoded.m_bits(), filter.m_bits());
        assert_eq!(decoded.k_hashes(), filter.k_hashes());
        assert_eq!(decoded.seed(), filter.seed());
        assert_eq!(decoded.bits(), filter.bits());
    }
}
