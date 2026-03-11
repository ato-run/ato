use crate::types::ChunkDescriptor;
use fastcdc::v2020::FastCDC;

/// Deterministically chunk payload bytes using FastCDC.
pub fn chunk_bytes_fastcdc(
    payload: &[u8],
    min_size: u32,
    avg_size: u32,
    max_size: u32,
) -> Vec<ChunkDescriptor> {
    if payload.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for entry in FastCDC::new(payload, min_size, avg_size, max_size) {
        let begin = entry.offset;
        let end = entry.offset + entry.length;
        let chunk = &payload[begin..end];
        let digest = blake3::hash(chunk);
        out.push(ChunkDescriptor {
            chunk_hash: format!("blake3:{}", digest.to_hex()),
            offset: begin as u64,
            length: entry.length as u64,
            codec: "fastcdc".to_string(),
            compression: "none".to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::chunk_bytes_fastcdc;

    #[test]
    fn deterministic_chunk_boundaries() {
        let data = vec![0x42u8; 512 * 1024];
        let first = chunk_bytes_fastcdc(&data, 16 * 1024, 64 * 1024, 256 * 1024);
        let second = chunk_bytes_fastcdc(&data, 16 * 1024, 64 * 1024, 256 * 1024);
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }
}
