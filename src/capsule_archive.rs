use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};

pub fn extract_payload_tar_from_capsule(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        if path.to_string_lossy() != "payload.tar.zst" {
            continue;
        }

        let mut payload_zst = Vec::new();
        entry
            .read_to_end(&mut payload_zst)
            .context("Failed to read payload.tar.zst from artifact")?;
        let mut decoder = zstd::stream::Decoder::new(Cursor::new(payload_zst))
            .context("Failed to decode payload.tar.zst")?;
        let mut payload_tar = Vec::new();
        decoder
            .read_to_end(&mut payload_tar)
            .context("Failed to read payload.tar bytes")?;
        return Ok(payload_tar);
    }

    bail!("Invalid artifact: payload.tar.zst not found in .capsule archive")
}

#[cfg(test)]
mod tests {
    use super::extract_payload_tar_from_capsule;
    use std::io::{Cursor, Write};

    fn build_capsule(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            for (path, contents) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, *path, Cursor::new(contents))
                    .expect("append tar entry");
            }
            builder.finish().expect("finish tar");
        }
        out
    }

    fn zstd_bytes(payload: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 0).expect("create zstd encoder");
        encoder.write_all(payload).expect("write zstd payload");
        encoder.finish().expect("finish zstd encoder")
    }

    #[test]
    fn extracts_payload_tar_from_capsule() {
        let payload_tar = build_capsule(&[("hello.txt", b"hello".to_vec())]);
        let capsule = build_capsule(&[("payload.tar.zst", zstd_bytes(&payload_tar))]);

        let extracted = extract_payload_tar_from_capsule(&capsule).expect("extract payload");

        assert_eq!(extracted, payload_tar);
    }

    #[test]
    fn errors_when_payload_tar_is_missing() {
        let capsule = build_capsule(&[("capsule.toml", b"name = \"demo\"".to_vec())]);
        let error = extract_payload_tar_from_capsule(&capsule).expect_err("missing payload");
        assert!(error
            .to_string()
            .contains("payload.tar.zst not found in .capsule archive"));
    }
}
