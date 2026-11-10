//! Minimal stored (uncompressed) ZIP writer/reader for saved-state files.
//!
//! The hosted DataResource export is a zip containing `metadata.json`,
//! `data.json` and optionally `schema.jsonc`, written at level 0. A stored zip
//! is therefore the interoperable form: readers do not need a decompressor and
//! the bytes are canonical. This module deliberately refuses anything else
//! (compression, data descriptors) instead of guessing.

use std::collections::BTreeMap;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const END_SIG: u32 = 0x0605_4b50;
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *slot = value;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// SHA-256 of arbitrary bytes as the `sha256:<hex>` form used on the wire.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Write a stored zip (method 0) with the entries in the given order.
pub fn write_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        push_u16(&mut out, 20);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u32(&mut out, crc);
        push_u32(&mut out, data.len() as u32);
        push_u32(&mut out, data.len() as u32);
        push_u16(&mut out, name.len() as u16);
        push_u16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, data.len() as u32);
        push_u32(&mut central, data.len() as u32);
        push_u16(&mut central, name.len() as u16);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&END_SIG.to_le_bytes());
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    out
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
        .ok_or_else(|| "zip_truncated".to_owned())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
        .ok_or_else(|| "zip_truncated".to_owned())
}

/// Read every stored entry. A compressed entry is refused (we never write one,
/// and silently mis-reading a Hosted archive would be worse than failing).
pub fn read_zip(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err("zip_too_large".to_owned());
    }
    let mut entries = BTreeMap::new();
    let mut offset = 0usize;
    while offset + 4 <= bytes.len() {
        let signature = read_u32(bytes, offset)?;
        if signature != LOCAL_SIG {
            break;
        }
        let flags = read_u16(bytes, offset + 6)?;
        let method = read_u16(bytes, offset + 8)?;
        if flags & 0x0008 != 0 {
            return Err("zip_data_descriptor_unsupported".to_owned());
        }
        if method != 0 {
            return Err("zip_compression_unsupported".to_owned());
        }
        let crc = read_u32(bytes, offset + 14)?;
        let compressed = read_u32(bytes, offset + 18)? as usize;
        let uncompressed = read_u32(bytes, offset + 22)? as usize;
        let name_len = read_u16(bytes, offset + 26)? as usize;
        let extra_len = read_u16(bytes, offset + 28)? as usize;
        let name_start = offset + 30;
        let name_end = name_start + name_len;
        let name = std::str::from_utf8(
            bytes
                .get(name_start..name_end)
                .ok_or_else(|| "zip_truncated".to_owned())?,
        )
        .map_err(|_| "zip_name_not_utf8".to_owned())?
        .to_owned();
        let data_start = name_end + extra_len;
        let data_end = data_start + compressed;
        let data = bytes
            .get(data_start..data_end)
            .ok_or_else(|| "zip_truncated".to_owned())?
            .to_vec();
        if compressed != uncompressed {
            return Err("zip_compression_unsupported".to_owned());
        }
        if data.len() > MAX_ENTRY_BYTES {
            return Err("zip_entry_too_large".to_owned());
        }
        if crc32(&data) != crc {
            return Err(format!("zip_crc_mismatch:{name}"));
        }
        entries.insert(name, data);
        offset = data_end;
    }
    if entries.is_empty() {
        return Err("zip_empty".to_owned());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_stored_entries() {
        let zip = write_zip(&[
            ("metadata.json", b"{\"a\":1}".as_slice()),
            ("data.json", b"{}".as_slice()),
        ]);
        let entries = read_zip(&zip).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries["data.json"], b"{}");
    }

    #[test]
    fn refuses_a_corrupt_archive() {
        let payload = b"{\"payload\":true}";
        let mut zip = write_zip(&[("data.json", payload.as_slice())]);
        // Flip one byte of the stored payload so the CRC no longer matches.
        let index = zip
            .windows(payload.len())
            .position(|window| window == payload)
            .expect("payload is stored verbatim");
        zip[index] ^= 0xFF;
        assert!(matches!(
            read_zip(&zip),
            Err(error) if error.starts_with("zip_crc_mismatch")
        ));
        assert!(read_zip(b"PK\x03\x04broken").is_err());
    }
}
