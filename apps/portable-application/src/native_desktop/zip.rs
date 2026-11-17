//! Safe materialization of a "zip" artifact.
//!
//! A remote native reference may point at a ".zip" instead of a ".tar.gz".
//! The bundle closure does not cover those bytes (the Run fetched them under a
//! pinned digest), so unpacking is the boundary that has to make the tree safe.
//! It reads the central directory, so the size, CRC32 and compression method of
//! every entry are the ones the archive author published, and then:
//!
//! * every name must be valid UTF-8, relative and traversal-free, with "/"
//!   separators; links, devices, fifos and encrypted entries are refused;
//! * only "stored" and "deflate" are decompressed. bzip2, zstd, lzma and
//!   ZIP64 are named refusals (native_format_extractor_unavailable), never a
//!   silent partial unpack;
//! * the decompressed bytes of every entry are bounded, the running total is
//!   bounded by the same artifact limit the tar.gz path uses, and the size and
//!   CRC32 in the central directory must match the bytes actually produced;
//! * the executable bits the archive records are preserved (with
//!   setuid/setgid/sticky stripped), so the launch stage sees the same tree a
//!   tar.gz would have produced, and an entry that records no UNIX mode is not
//!   made executable by inference.
//!
//! Every entry is checked before anything is placed, so an archive that names
//! one unsafe path leaves the artifact root untouched. Nothing here invents a
//! temporary file: the tree is written where the launch stage expects it.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use sha2::{Digest, Sha256};

use super::artifact::{
    ExtractedArtifact, MAX_EXTRACTED_BYTES, MAX_EXTRACTED_ENTRIES, PlacedFile, finish,
    safe_relative, set_mode, slash_path,
};

/// The end-of-central-directory record.
const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
/// One central directory header.
const CENTRAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
/// One local file header.
const LOCAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
const EOCD_BYTES: u64 = 22;
const CENTRAL_HEADER_BYTES: usize = 46;
const LOCAL_HEADER_BYTES: u64 = 30;
/// The largest tail that can carry an end-of-central-directory record plus its
/// maximum comment.
const MAX_EOCD_SEARCH_BYTES: u64 = EOCD_BYTES + u16::MAX as u64;
/// No compression.
const METHOD_STORED: u16 = 0;
/// RFC 1951 deflate, the one compressed method this build decompresses.
const METHOD_DEFLATE: u16 = 8;
/// The st_mode type field inside a ZIP external-attributes word.
const FILE_TYPE_MASK: u32 = 0o170000;
const FILE_TYPE_REGULAR: u32 = 0o100000;
const FILE_TYPE_DIRECTORY: u32 = 0o040000;
const FILE_TYPE_SYMLINK: u32 = 0o120000;
/// The permission bits of a UNIX mode. The setuid, setgid and sticky bits are
/// deliberately outside it, so they are stripped rather than restored.
const PERMISSION_MASK: u32 = 0o777;
const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIRECTORY_MODE: u32 = 0o755;

/// Unpack a zip artifact held in memory under "destination", which must not
/// exist yet.
pub fn extract_zip(archive: &[u8], destination: &Path) -> Result<ExtractedArtifact> {
    extract_zip_source(std::io::Cursor::new(archive), destination)
}

/// Unpack a zip artifact on disk, seeking through it instead of holding it.
pub fn extract_zip_file(path: &Path, destination: &Path) -> Result<ExtractedArtifact> {
    let file = File::open(path)
        .with_context(|| format!("open the verified payload {}", path.display()))?;
    extract_zip_source(BufReader::new(file), destination)
}

/// A refusal for a well-formed archive this build cannot unpack.
///
/// The stable prefix is how the launch stage keeps an unsupported packing
/// apart from a corrupted artifact; the detail names what this build would have
/// needed.
fn unsupported(detail: String) -> anyhow::Error {
    anyhow!("native_format_extractor_unavailable: {detail}")
}

fn extract_zip_source<R: Read + Seek>(
    mut source: R,
    destination: &Path,
) -> Result<ExtractedArtifact> {
    ensure!(
        !destination.exists(),
        "artifact destination already exists: {}",
        destination.display()
    );
    let length = source
        .seek(SeekFrom::End(0))
        .context("measure the native artifact")?;
    // Plan every entry first: a refused name or type must leave no tree behind.
    let planned = plan(read_central_directory(&mut source, length)?)?;
    ensure!(
        planned.iter().any(|entry| !entry.directory),
        "the native artifact carries no regular file"
    );
    fs::create_dir_all(destination)
        .with_context(|| format!("create artifact root {}", destination.display()))?;

    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for entry in &planned {
        let output = destination.join(&entry.relative);
        if entry.directory {
            fs::create_dir_all(&output)
                .with_context(|| format!("create artifact directory {}", output.display()))?;
            set_mode(&output, entry.mode & 0o755)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create artifact directory {}", parent.display()))?;
        }
        let (size, digest) = write_entry(&mut source, entry, &output, length)?;
        total_bytes = total_bytes.saturating_add(size);
        set_mode(&output, entry.mode)?;
        files.push(PlacedFile {
            path: slash_path(&entry.relative),
            mode: entry.mode,
            size,
            digest,
        });
    }
    finish(destination, files, total_bytes)
}

/// One central directory record, before anything has been placed.
struct RawEntry {
    name: String,
    method: u16,
    crc32: u32,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
    external: u32,
}

/// One entry that passed every check and will be placed.
struct PlannedEntry {
    name: String,
    relative: PathBuf,
    mode: u32,
    method: u16,
    crc32: u32,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
    directory: bool,
}

/// Read the central directory from the end of the payload.
///
/// The directory is read, not the local headers, because it is the record the
/// writer finalized: it carries the size, CRC32 and method an entry is checked
/// against, and reading it first lets the whole archive be refused before a
/// byte is placed.
fn read_central_directory<R: Read + Seek>(source: &mut R, length: u64) -> Result<Vec<RawEntry>> {
    ensure!(
        length >= EOCD_BYTES,
        "the native artifact is too short to be a zip archive"
    );
    let tail_length = length.min(MAX_EOCD_SEARCH_BYTES);
    let mut tail = vec![0_u8; tail_length as usize];
    read_exact_at(source, length - tail_length, &mut tail)?;
    let record = find_eocd(&tail).ok_or_else(|| {
        anyhow!(
            "the native artifact is not a readable zip archive: it carries no \
             end-of-central-directory record"
        )
    })?;
    let eocd = &tail[record..];
    let disk = le_u16(&eocd[4..6]);
    let directory_disk = le_u16(&eocd[6..8]);
    let entries_on_disk = le_u16(&eocd[8..10]);
    let total = le_u16(&eocd[10..12]);
    let directory_size = le_u32(&eocd[12..16]);
    let directory_offset = le_u32(&eocd[16..20]);
    ensure!(
        disk == 0 && directory_disk == 0 && entries_on_disk == total,
        "the native artifact is a multi-disk zip archive, which this Adapter does not materialize"
    );
    // 0xFFFF and 0xFFFF_FFFF are the ZIP64 sentinels: the real record is in a
    // ZIP64 structure this build does not read. Refusing beats guessing.
    if total == u16::MAX || directory_size == u32::MAX || directory_offset == u32::MAX {
        return Err(unsupported(
            "the zip archive is a ZIP64 archive, and this build does not unpack ZIP64".to_owned(),
        ));
    }
    ensure!(
        u64::from(total) <= MAX_EXTRACTED_ENTRIES,
        "the native artifact carries more than {MAX_EXTRACTED_ENTRIES} entries"
    );
    let directory_size = u64::from(directory_size);
    let directory_offset = u64::from(directory_offset);
    ensure!(
        directory_offset
            .checked_add(directory_size)
            .is_some_and(|end| end <= length),
        "the zip central directory lies outside the payload"
    );
    source
        .seek(SeekFrom::Start(directory_offset))
        .context("seek the zip central directory")?;
    let mut entries = Vec::with_capacity(total as usize);
    {
        let mut reader = BufReader::new((&mut *source).take(directory_size));
        for index in 0..total {
            entries.push(read_central_entry(&mut reader, index)?);
        }
    }
    Ok(entries)
}

/// The last end-of-central-directory record whose comment ends the payload.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < EOCD_BYTES as usize {
        return None;
    }
    let last = tail.len() - EOCD_BYTES as usize;
    for offset in (0..=last).rev() {
        if tail[offset..offset + 4] != EOCD_SIGNATURE {
            continue;
        }
        let comment = le_u16(&tail[offset + 20..offset + 22]) as usize;
        if offset + EOCD_BYTES as usize + comment == tail.len() {
            return Some(offset);
        }
    }
    None
}

fn read_central_entry<R: Read>(reader: &mut R, index: u16) -> Result<RawEntry> {
    let mut header = [0_u8; CENTRAL_HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .with_context(|| format!("read zip central directory entry {index}"))?;
    ensure!(
        header[..4] == CENTRAL_SIGNATURE,
        "zip central directory entry {index} has the wrong signature"
    );
    let flags = le_u16(&header[8..10]);
    ensure!(
        flags & 0x0001 == 0 && flags & 0x0040 == 0,
        "zip central directory entry {index} is encrypted, and this Adapter does not \
         materialize encrypted entries"
    );
    let method = le_u16(&header[10..12]);
    let crc32 = le_u32(&header[16..20]);
    let compressed = le_u32(&header[20..24]);
    let uncompressed = le_u32(&header[24..28]);
    let name_length = le_u16(&header[28..30]) as usize;
    let extra_length = u64::from(le_u16(&header[30..32]));
    let comment_length = u64::from(le_u16(&header[32..34]));
    let disk_start = le_u16(&header[34..36]);
    ensure!(
        disk_start == 0,
        "zip central directory entry {index} starts on another disk"
    );
    let external = le_u32(&header[38..42]);
    let local_offset = le_u32(&header[42..46]);
    if compressed == u32::MAX || uncompressed == u32::MAX || local_offset == u32::MAX {
        return Err(unsupported(format!(
            "zip entry {index} is a ZIP64 entry, and this build does not unpack ZIP64"
        )));
    }
    let mut name = vec![0_u8; name_length];
    reader
        .read_exact(&mut name)
        .with_context(|| format!("read the name of zip central directory entry {index}"))?;
    let skip = extra_length + comment_length;
    let skipped = std::io::copy(&mut reader.take(skip), &mut std::io::sink())
        .with_context(|| format!("skip the extra fields of zip central directory entry {index}"))?;
    ensure!(
        skipped == skip,
        "zip central directory entry {index} is truncated"
    );
    let name = String::from_utf8(name)
        .with_context(|| format!("zip central directory entry {index} is not a UTF-8 name"))?;
    Ok(RawEntry {
        name,
        method,
        crc32,
        compressed: u64::from(compressed),
        uncompressed: u64::from(uncompressed),
        local_offset: u64::from(local_offset),
        external,
    })
}

/// Check every entry before the first byte is placed.
fn plan(entries: Vec<RawEntry>) -> Result<Vec<PlannedEntry>> {
    ensure!(!entries.is_empty(), "the native artifact carries no entry");
    let mut planned = Vec::with_capacity(entries.len());
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for entry in entries {
        let RawEntry {
            name,
            method,
            crc32,
            compressed,
            uncompressed,
            local_offset,
            external,
        } = entry;
        ensure!(
            method == METHOD_STORED || method == METHOD_DEFLATE,
            "native_format_extractor_unavailable: zip entry {name} uses compression method \
             {method}, and this build decompresses stored and deflate only"
        );
        ensure!(
            !name.contains('\\'),
            "zip entry {name} names a path with a backslash separator"
        );
        ensure!(
            !name.is_empty(),
            "the zip archive carries an entry with no name"
        );
        let file_type = (external >> 16) & FILE_TYPE_MASK;
        let trailing_slash = name.ends_with('/');
        let directory = match file_type {
            FILE_TYPE_SYMLINK => bail!(
                "zip entry {name} is a symbolic link; only regular files and directories \
                 are materialized"
            ),
            FILE_TYPE_DIRECTORY => true,
            FILE_TYPE_REGULAR | 0 => trailing_slash,
            other => {
                bail!("zip entry {name} is not a regular file or a directory (UNIX type {other:o})")
            }
        };
        ensure!(
            !(file_type == FILE_TYPE_REGULAR && trailing_slash),
            "zip entry {name} is a regular file with a directory name"
        );
        let name = name.trim_end_matches('/');
        ensure!(
            !name.is_empty(),
            "the zip archive carries an entry whose name is only separators"
        );
        let relative =
            safe_relative(Path::new(name)).with_context(|| format!("zip entry {name}"))?;
        ensure!(
            !relative.as_os_str().is_empty(),
            "zip entry {name} names no path"
        );
        ensure!(
            seen.insert(relative.clone()),
            "the zip artifact carries {} more than once",
            relative.display()
        );
        let recorded_mode = (external >> 16) & PERMISSION_MASK;
        let mode = match (directory, recorded_mode) {
            (true, 0) => DEFAULT_DIRECTORY_MODE,
            (true, mode) => mode & 0o755,
            (false, 0) => DEFAULT_FILE_MODE,
            (false, mode) => mode,
        };
        if !directory {
            total = total.saturating_add(uncompressed);
            ensure!(
                uncompressed <= MAX_EXTRACTED_BYTES,
                "zip entry {name} expands past {MAX_EXTRACTED_BYTES} bytes"
            );
            ensure!(
                total <= MAX_EXTRACTED_BYTES,
                "the native artifact expands past {MAX_EXTRACTED_BYTES} bytes"
            );
        }
        planned.push(PlannedEntry {
            name: name.to_owned(),
            relative,
            mode,
            method,
            crc32,
            compressed,
            uncompressed,
            local_offset,
            directory,
        });
    }
    Ok(planned)
}

/// Place one regular file and return its size and content digest.
fn write_entry<R: Read + Seek>(
    source: &mut R,
    entry: &PlannedEntry,
    output: &Path,
    length: u64,
) -> Result<(u64, String)> {
    let start = local_data_offset(source, entry, length)?;
    source
        .seek(SeekFrom::Start(start))
        .with_context(|| format!("seek the data of zip entry {}", entry.name))?;
    let mut file = File::create(output)
        .with_context(|| format!("create artifact file {}", output.display()))?;
    let mut crc = flate2::Crc::new();
    let mut hasher = Sha256::new();
    let mut written = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    match entry.method {
        METHOD_STORED => {
            ensure!(
                entry.compressed == entry.uncompressed,
                "zip entry {} is stored but declares different compressed and uncompressed \
                 sizes",
                entry.name
            );
            let mut payload = (&mut *source).take(entry.compressed);
            loop {
                let read = payload
                    .read(&mut buffer)
                    .with_context(|| format!("read zip entry {}", entry.name))?;
                if read == 0 {
                    break;
                }
                written = written.saturating_add(read as u64);
                ensure!(
                    written <= entry.uncompressed,
                    "zip entry {} is longer than its declared size",
                    entry.name
                );
                crc.update(&buffer[..read]);
                hasher.update(&buffer[..read]);
                file.write_all(&buffer[..read])
                    .with_context(|| format!("write artifact file {}", output.display()))?;
            }
        }
        METHOD_DEFLATE => {
            let payload = (&mut *source).take(entry.compressed);
            let mut decoder = flate2::bufread::DeflateDecoder::new(BufReader::new(payload));
            loop {
                let read = decoder
                    .read(&mut buffer)
                    .with_context(|| format!("decompress zip entry {}", entry.name))?;
                if read == 0 {
                    break;
                }
                written = written.saturating_add(read as u64);
                ensure!(
                    written <= entry.uncompressed,
                    "zip entry {} expands past its declared size",
                    entry.name
                );
                crc.update(&buffer[..read]);
                hasher.update(&buffer[..read]);
                file.write_all(&buffer[..read])
                    .with_context(|| format!("write artifact file {}", output.display()))?;
            }
            // The declared compressed size must be the bytes the stream used,
            // not a prefix of a larger blob this Adapter would silently ignore.
            ensure!(
                decoder.get_ref().get_ref().limit() == 0,
                "zip entry {} declares more compressed bytes than its stream uses",
                entry.name
            );
        }
        other => bail!(
            "native_format_extractor_unavailable: zip entry {} uses compression method \
             {other}, and this build decompresses stored and deflate only",
            entry.name
        ),
    }
    drop(file);
    ensure!(
        written == entry.uncompressed,
        "zip entry {} is shorter than its declared size",
        entry.name
    );
    ensure!(
        crc.sum() == entry.crc32,
        "zip entry {} fails its CRC32 check",
        entry.name
    );
    Ok((written, format!("{:x}", hasher.finalize())))
}

/// Where one entry's bytes start, after proving the local header agrees.
///
/// The central directory says where the local header is; the local header says
/// how long its own name and extra field are. Comparing the two names keeps a
/// local record from pointing the data at different bytes than the directory
/// described.
fn local_data_offset<R: Read + Seek>(
    source: &mut R,
    entry: &PlannedEntry,
    length: u64,
) -> Result<u64> {
    let mut header = [0_u8; LOCAL_HEADER_BYTES as usize];
    read_exact_at(source, entry.local_offset, &mut header)?;
    ensure!(
        header[..4] == LOCAL_SIGNATURE,
        "zip entry {} has no local header at its recorded offset",
        entry.name
    );
    let name_length = u64::from(le_u16(&header[26..28]));
    let extra_length = u64::from(le_u16(&header[28..30]));
    let mut local_name = vec![0_u8; name_length as usize];
    read_exact_at(
        source,
        entry.local_offset + LOCAL_HEADER_BYTES,
        &mut local_name,
    )?;
    let expected = entry.name.as_bytes();
    let declared = local_name.strip_suffix(b"/").unwrap_or(&local_name);
    ensure!(
        declared == expected,
        "zip entry {} has a local header that names different bytes",
        entry.name
    );
    let start = entry.local_offset + LOCAL_HEADER_BYTES + name_length + extra_length;
    ensure!(
        start
            .checked_add(entry.compressed)
            .is_some_and(|end| end <= length),
        "zip entry {} extends past the payload",
        entry.name
    );
    Ok(start)
}

fn read_exact_at<R: Read + Seek>(source: &mut R, offset: u64, buffer: &mut [u8]) -> Result<()> {
    source
        .seek(SeekFrom::Start(offset))
        .with_context(|| format!("seek the native artifact to {offset}"))?;
    source.read_exact(buffer).with_context(|| {
        format!(
            "read {} bytes at {offset} from the native artifact",
            buffer.len()
        )
    })
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;

    /// One entry of a zip this test writes by hand.
    ///
    /// The archive is built instead of committed: a stored and a deflated
    /// entry, a mismatching CRC and a lying size are all facts about bytes, and
    /// a committed fixture would hide which one a test exercises.
    struct TestEntry {
        name: String,
        data: Vec<u8>,
        method: u16,
        flags: u16,
        /// The 16-bit UNIX mode written into the external attributes, including
        /// the file-type bits.
        mode: u32,
        crc: Option<u32>,
        declared_uncompressed: Option<u32>,
        declared_compressed: Option<u32>,
    }

    impl TestEntry {
        fn file(name: &str, data: &[u8], mode: u32) -> Self {
            Self {
                name: name.to_owned(),
                data: data.to_vec(),
                method: METHOD_STORED,
                flags: 0,
                mode,
                crc: None,
                declared_uncompressed: None,
                declared_compressed: None,
            }
        }

        fn deflated(name: &str, data: &[u8], mode: u32) -> Self {
            Self {
                method: METHOD_DEFLATE,
                ..Self::file(name, data, mode)
            }
        }

        fn directory(name: &str) -> Self {
            Self {
                mode: FILE_TYPE_DIRECTORY | 0o755,
                ..Self::file(name, b"", 0)
            }
        }

        fn symlink(name: &str, target: &str) -> Self {
            Self {
                mode: FILE_TYPE_SYMLINK | 0o777,
                ..Self::file(name, target.as_bytes(), 0)
            }
        }

        fn fifo(name: &str) -> Self {
            Self {
                mode: 0o010644,
                ..Self::file(name, b"", 0)
            }
        }

        fn encrypted(name: &str) -> Self {
            Self {
                flags: 0x0001,
                ..Self::file(name, b"", 0o100644)
            }
        }

        fn with_method(mut self, method: u16) -> Self {
            self.method = method;
            self
        }

        fn with_crc(mut self, crc: u32) -> Self {
            self.crc = Some(crc);
            self
        }

        fn declaring_uncompressed(mut self, bytes: u32) -> Self {
            self.declared_uncompressed = Some(bytes);
            self
        }
    }

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        crc.sum()
    }

    fn deflate(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// Write a well-formed archive: local headers, the data, then the central
    /// directory the extractor reads.
    fn build(entries: &[TestEntry]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut directory = Vec::new();
        for entry in entries {
            let payload = match entry.method {
                METHOD_DEFLATE => deflate(&entry.data),
                _ => entry.data.clone(),
            };
            let crc = entry.crc.unwrap_or_else(|| crc32(&entry.data));
            let uncompressed = entry
                .declared_uncompressed
                .unwrap_or(entry.data.len() as u32);
            let compressed = entry.declared_compressed.unwrap_or(payload.len() as u32);
            let name = entry.name.as_bytes();
            let offset = body.len() as u32;

            body.extend_from_slice(&LOCAL_SIGNATURE);
            push_u16(&mut body, 20);
            push_u16(&mut body, entry.flags);
            push_u16(&mut body, entry.method);
            push_u16(&mut body, 0);
            push_u16(&mut body, 0x21);
            push_u32(&mut body, crc);
            push_u32(&mut body, compressed);
            push_u32(&mut body, uncompressed);
            push_u16(&mut body, name.len() as u16);
            push_u16(&mut body, 0);
            body.extend_from_slice(name);
            body.extend_from_slice(&payload);

            directory.extend_from_slice(&CENTRAL_SIGNATURE);
            push_u16(&mut directory, 0x031e);
            push_u16(&mut directory, 20);
            push_u16(&mut directory, entry.flags);
            push_u16(&mut directory, entry.method);
            push_u16(&mut directory, 0);
            push_u16(&mut directory, 0x21);
            push_u32(&mut directory, crc);
            push_u32(&mut directory, compressed);
            push_u32(&mut directory, uncompressed);
            push_u16(&mut directory, name.len() as u16);
            push_u16(&mut directory, 0);
            push_u16(&mut directory, 0);
            push_u16(&mut directory, 0);
            push_u16(&mut directory, 0);
            push_u32(&mut directory, entry.mode << 16);
            push_u32(&mut directory, offset);
            directory.extend_from_slice(name);
        }
        let directory_offset = body.len() as u32;
        let directory_size = directory.len() as u32;
        body.extend_from_slice(&directory);
        body.extend_from_slice(&EOCD_SIGNATURE);
        push_u16(&mut body, 0);
        push_u16(&mut body, 0);
        push_u16(&mut body, entries.len() as u16);
        push_u16(&mut body, entries.len() as u16);
        push_u32(&mut body, directory_size);
        push_u32(&mut body, directory_offset);
        push_u16(&mut body, 0);
        body
    }

    /// Turn the built archive into one that declares the ZIP64 marker.
    fn declare_zip64(bytes: &mut [u8]) {
        let offset = find_eocd(bytes).expect("a built archive carries an end record");
        // A real ZIP64 archive writes the sentinel into both the per-disk and
        // the total count, so the copy is patched the same way.
        bytes[offset + 8..offset + 10].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes[offset + 10..offset + 12].copy_from_slice(&u16::MAX.to_le_bytes());
    }

    fn root(scratch: &tempfile::TempDir) -> PathBuf {
        scratch.path().join("artifact")
    }

    #[test]
    fn stored_and_deflated_entries_are_placed_with_their_modes_and_a_stable_digest() {
        let body = "#!/bin/sh\nexit 0\n";
        let bytes = build(&[
            TestEntry::directory("App.app/"),
            TestEntry::file("App.app/Contents/MacOS/App", body.as_bytes(), 0o100755),
            TestEntry::deflated("App.app/Contents/Info.plist", b"<plist/>", 0o100644),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let destination = root(&scratch);
        let extracted = extract_zip(&bytes, &destination).unwrap();
        assert_eq!(extracted.file_count, 2);
        assert_eq!(
            extracted.total_bytes,
            (body.len() + b"<plist/>".len()) as u64
        );
        assert_eq!(
            fs::read(destination.join("App.app/Contents/Info.plist")).unwrap(),
            b"<plist/>"
        );
        let entry =
            crate::native_desktop::resolve_entry(&extracted.root, "App.app/Contents/MacOS/App")
                .unwrap();
        assert!(entry.starts_with(destination.canonicalize().unwrap()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(destination.join("App.app/Contents/MacOS/App"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755, "the recorded executable bit was not preserved");
        }
        assert!(extracted.tree_digest.starts_with("sha256:"));

        // Entry order does not change the materialized digest.
        let reordered = build(&[
            TestEntry::deflated("App.app/Contents/Info.plist", b"<plist/>", 0o100644),
            TestEntry::file("App.app/Contents/MacOS/App", body.as_bytes(), 0o100755),
            TestEntry::directory("App.app/"),
        ]);
        let other = extract_zip(&reordered, &scratch.path().join("reordered")).unwrap();
        assert_eq!(extracted.tree_digest, other.tree_digest);

        // A payload on disk is seeked through and produces the same tree, with
        // no temporary file left beside it.
        let payload = scratch.path().join("artifact.zip");
        fs::write(&payload, &bytes).unwrap();
        let from_disk = extract_zip_file(&payload, &scratch.path().join("disk")).unwrap();
        assert_eq!(extracted.tree_digest, from_disk.tree_digest);
        assert!(!payload.with_extension("part").exists());
    }

    #[test]
    fn escaping_names_are_refused_before_anything_is_placed() {
        for name in [
            "../evil",
            "..",
            "App/../../evil",
            "/etc/evil",
            "App\\..\\evil",
        ] {
            let bytes = build(&[TestEntry::file(name, b"x", 0o100644)]);
            let scratch = tempfile::tempdir().unwrap();
            let destination = root(&scratch);
            let error = extract_zip(&bytes, &destination).unwrap_err();
            assert!(
                !destination.exists(),
                "{name}: a refused archive created the artifact root"
            );
            assert!(
                !scratch.path().join("evil").exists(),
                "{name}: an escaping entry was placed"
            );
            assert!(
                error.to_string().contains("zip") || error.to_string().contains("entry"),
                "{name}: unexpected error {error}"
            );
        }

        // A safe name and an unsafe one in the same archive still places
        // nothing: the plan is checked before the first write.
        let bytes = build(&[
            TestEntry::file("App/run", b"x", 0o100755),
            TestEntry::file("App/../../evil", b"x", 0o100644),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let destination = root(&scratch);
        assert!(extract_zip(&bytes, &destination).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn links_and_special_entries_are_refused() {
        for (what, entry) in [
            ("a symlink", TestEntry::symlink("App/link", "/etc/passwd")),
            (
                "a relative symlink",
                TestEntry::symlink("App/link", "../../outside"),
            ),
            (
                "a symlinked directory",
                TestEntry::symlink("App/dir/", "/tmp"),
            ),
            ("a fifo", TestEntry::fifo("App/pipe")),
            ("an encrypted entry", TestEntry::encrypted("App/run")),
        ] {
            let bytes = build(&[entry]);
            let scratch = tempfile::tempdir().unwrap();
            let destination = root(&scratch);
            let error = extract_zip(&bytes, &destination).unwrap_err();
            assert!(
                !destination.exists(),
                "{what}: a refused archive created the artifact root"
            );
            assert!(
                !destination.join("App/link").exists() && !destination.join("App/pipe").exists(),
                "{what}: a refused entry was placed"
            );
            assert!(
                error.to_string().contains("zip"),
                "{what}: unexpected error {error}"
            );
        }
    }

    #[test]
    fn an_archive_that_declares_more_than_it_may_expand_is_refused() {
        let too_large = build(&[TestEntry::file("big", b"x", 0o100644)
            .declaring_uncompressed(u32::try_from(MAX_EXTRACTED_BYTES + 1).unwrap())]);
        let scratch = tempfile::tempdir().unwrap();
        let destination = root(&scratch);
        let error = extract_zip(&too_large, &destination).unwrap_err();
        assert!(error.to_string().contains("expands past"), "{error}");
        assert!(!destination.exists());

        // A declared size smaller than the stream and one larger than it are
        // both refusals, so a lying central directory is never trusted.
        let smaller =
            build(&[TestEntry::deflated("short", b"01234567", 0o100644).declaring_uncompressed(4)]);
        let error = extract_zip(&smaller, &scratch.path().join("smaller")).unwrap_err();
        assert!(error.to_string().contains("expands past"), "{error}");

        let larger =
            build(&[TestEntry::deflated("long", b"0123", 0o100644).declaring_uncompressed(8)]);
        let error = extract_zip(&larger, &scratch.path().join("larger")).unwrap_err();
        assert!(error.to_string().contains("shorter"), "{error}");
    }

    #[test]
    fn a_crc_that_does_not_match_the_bytes_is_refused() {
        let bytes = build(&[TestEntry::file("App/run", b"#!/bin/sh\n", 0o100755).with_crc(0)]);
        let scratch = tempfile::tempdir().unwrap();
        let error = extract_zip(&bytes, &root(&scratch)).unwrap_err();
        assert!(error.to_string().contains("CRC32"), "{error}");
    }

    #[test]
    fn a_missing_or_non_executable_entry_is_reported_by_the_launch_stage() {
        let bytes = build(&[TestEntry::file("App/run", b"x", 0o100755)]);
        let scratch = tempfile::tempdir().unwrap();
        let extracted = extract_zip(&bytes, &root(&scratch)).unwrap();
        assert_eq!(
            crate::native_desktop::resolve_entry(&extracted.root, "App/other")
                .unwrap_err()
                .code,
            "native_entry_missing"
        );

        // A zip that records no executable bit is not made executable by
        // inference: the same rule the tar.gz path applies refuses it.
        let plain = build(&[TestEntry::file("App/run", b"x", 0o100644)]);
        let plain_root = scratch.path().join("plain");
        let extracted = extract_zip(&plain, &plain_root).unwrap();
        assert_eq!(
            crate::native_desktop::resolve_entry(&extracted.root, "App/run")
                .unwrap_err()
                .code,
            "native_entry_not_executable"
        );
    }

    #[test]
    fn an_unsupported_compression_method_is_named_as_unavailable() {
        // 12 is bzip2: a well-formed archive this build does not decompress.
        let bytes = build(&[TestEntry::file("App/run", b"x", 0o100644).with_method(12)]);
        let scratch = tempfile::tempdir().unwrap();
        let destination = root(&scratch);
        let error = extract_zip(&bytes, &destination).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("native_format_extractor_unavailable"),
            "{error}"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn a_zip64_archive_is_named_as_unavailable() {
        let mut bytes = build(&[TestEntry::file("App/run", b"x", 0o100644)]);
        declare_zip64(&mut bytes);
        let scratch = tempfile::tempdir().unwrap();
        let error = extract_zip(&bytes, &root(&scratch)).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("native_format_extractor_unavailable"),
            "{error}"
        );
    }

    #[test]
    fn a_duplicate_entry_and_an_empty_archive_are_refused() {
        let duplicate = build(&[
            TestEntry::file("App/run", b"a", 0o100644),
            TestEntry::file("App/run", b"b", 0o100644),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let error = extract_zip(&duplicate, &root(&scratch)).unwrap_err();
        assert!(error.to_string().contains("more than once"), "{error}");

        let empty = build(&[]);
        let error = extract_zip(&empty, &scratch.path().join("empty")).unwrap_err();
        assert!(error.to_string().contains("no entry"), "{error}");

        let not_an_archive = b"not a zip";
        assert!(extract_zip(not_an_archive, &scratch.path().join("other")).is_err());
    }

    #[test]
    fn a_destination_that_exists_is_never_overwritten() {
        let bytes = build(&[TestEntry::file("App/run", b"a", 0o100644)]);
        let scratch = tempfile::tempdir().unwrap();
        let destination = root(&scratch);
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep"), b"keep").unwrap();
        assert!(extract_zip(&bytes, &destination).is_err());
        assert_eq!(fs::read(destination.join("keep")).unwrap(), b"keep");
    }

    /// Serve exactly one request with a canned response and close.
    fn serve(status: &str, body: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;

        let status = status.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{address}/artifact.zip")
    }

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn a_remote_zip_is_only_unpacked_after_its_digest_matches() {
        use crate::native_desktop::remote::{FetchFailure, ReqwestFetcher, fetch_to_file};
        use ato_formation::native_desktop::{NativeArtifactFormat, RemoteArtifact};

        let bytes = build(&[TestEntry::file("App/run", b"#!/bin/sh\n", 0o100755)]);
        let scratch = tempfile::tempdir().unwrap();

        // The matching body is fetched, then unpacked in the declared format.
        let url = serve("200 OK", bytes.clone());
        let matching = RemoteArtifact {
            url,
            sha256: digest(&bytes),
            bytes: bytes.len() as u64,
        };
        let payload = scratch.path().join("fetch/artifact");
        let fetched = fetch_to_file(&matching, &payload, &ReqwestFetcher::default()).unwrap();
        let extracted = crate::native_desktop::artifact::extract_archive_file(
            &fetched.path,
            NativeArtifactFormat::Zip,
            &scratch.path().join("artifact"),
        )
        .unwrap();
        assert_eq!(extracted.file_count, 1);
        assert!(!payload.with_extension("part").exists());

        // The same bytes under a different digest never reach the extractor.
        let url = serve("200 OK", bytes.clone());
        let changed = RemoteArtifact {
            url,
            sha256: format!("sha256:{}", "0".repeat(64)),
            bytes: bytes.len() as u64,
        };
        let refused = scratch.path().join("changed/artifact");
        let error = fetch_to_file(&changed, &refused, &ReqwestFetcher::default()).unwrap_err();
        assert!(
            matches!(error, FetchFailure::DigestMismatch { .. }),
            "{error}"
        );
        assert!(!refused.exists());
        assert!(!refused.with_extension("part").exists());

        // A removed asset is not found, not a broken archive.
        let url = serve("404 Not Found", b"gone".to_vec());
        let gone_remote = RemoteArtifact {
            url,
            sha256: digest(&bytes),
            bytes: bytes.len() as u64,
        };
        let gone = scratch.path().join("gone/artifact");
        let error = fetch_to_file(&gone_remote, &gone, &ReqwestFetcher::default()).unwrap_err();
        assert_eq!(error, FetchFailure::NotFound(404), "{error}");
        assert!(!gone.exists());
    }
}
