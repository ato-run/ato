//! Capsulehub distribution wire format and client-side encryption.
//!
//! A capsulehub object is an opaque ciphertext blob addressed by the SHA-256
//! digest of its own bytes. The coordinator that stores it never learns the
//! Capsule content or the link key: encryption happens locally, and the key
//! lives only in the shared link fragment.
//!
//! This module deliberately interprets nothing about Capsule content. It does
//! not depend on the portable-application validator, does not decode bundles,
//! and makes no identity or admission decision. It only:
//!
//! - defines wire v1 (magic, version, secretstream header, length-prefixed
//!   `crypto_secretstream` frames),
//! - encrypts/decrypts with that wire as a stream (bounded memory),
//! - derives the object id from the complete ciphertext, and
//! - parses/builds the strict shared link.
//!
//! ## Wire v1
//!
//! ```text
//! offset 0..6   magic   = b"ATOHUB"
//! offset 6      version = 0x01
//! offset 7..31  secretstream header (24 random bytes)
//! offset 31..   frames: [u32 little-endian body_len][body]
//! ```
//!
//! `body` is one `PushStream` message: plaintext chunk + tag byte + 16-byte
//! MAC. Frame associated data is the 31-byte prefix
//! (`magic + version + header`), which binds the header. All frames but the
//! last carry `Tag::Message`; the last carries `Tag::Final`. Empty plaintext is
//! one empty `Tag::Final` frame.
//!
//! The key is a 32-byte random secret encoded as unpadded base64url in the
//! fragment (`#k=...`). Fragments are never sent in HTTP requests, so a hub
//! server never receives key material. The link also carries the claimed
//! `cid` (`root_contract_ref`), which the client recomputes after decryption;
//! this keeps the coordinator from swapping contents between ids it serves.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crypto_secretstream::{Header, Key, PullStream, PushStream, Tag};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Versioned capsulehub wire format emitted and accepted by this module.
pub const HUB_WIRE_VERSION: u8 = 1;
/// Magic prefix of every capsulehub ciphertext object.
pub const HUB_MAGIC: [u8; 6] = *b"ATOHUB";
/// Length of the `crypto_secretstream` header in wire v1.
pub const HUB_SECRETSTREAM_HEADER_BYTES: usize = 24;
/// Plaintext chunk size for every frame except the last.
pub const HUB_CHUNK_BYTES: usize = 1024 * 1024;
/// Maximum plaintext bytes a single object may carry.
pub const MAX_HUB_PLAINTEXT_BYTES: u64 = 768 * 1024 * 1024;

/// Length of the object id prefix (`sha256:`).
pub const HUB_OBJECT_ID_PREFIX: &str = "sha256:";

/// Magic + version + secretstream header, and the frame associated data.
const HUB_PREFIX_BYTES: usize = HUB_MAGIC.len() + 1 + HUB_SECRETSTREAM_HEADER_BYTES;
/// Tag byte + Poly1305 MAC added to each frame body.
const HUB_FRAME_OVERHEAD_BYTES: usize = 1 + 16;
const HUB_MIN_FRAME_BODY_BYTES: usize = HUB_FRAME_OVERHEAD_BYTES;
const HUB_MAX_FRAME_BODY_BYTES: usize = HUB_CHUNK_BYTES + HUB_FRAME_OVERHEAD_BYTES;
const HUB_SHA256_HEX_LEN: usize = 64;
const HUB_KEY_BYTES: usize = 32;
const HUB_KEY_FRAGMENT_BYTES: usize = (HUB_KEY_BYTES * 4).div_ceil(3);

#[derive(Debug, Error)]
pub enum HubError {
    #[error("capsulehub I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("capsulehub object does not start with the `ATOHUB` magic")]
    BadMagic,
    #[error("unsupported capsulehub wire version {0}")]
    UnsupportedVersion(u8),
    #[error("invalid capsulehub frame: {0}")]
    InvalidFrame(&'static str),
    #[error("capsulehub object is truncated")]
    Truncated,
    #[error("capsulehub object ends without a final frame")]
    MissingFinal,
    #[error("capsulehub object has data after its final frame")]
    TrailingData,
    #[error("capsulehub ciphertext authentication failed")]
    Authentication,
    #[error("capsulehub plaintext exceeds the maximum object size")]
    TooLarge,
    #[error("capsulehub object id mismatch: expected {expected}, got {actual}")]
    ObjectIdMismatch { expected: String, actual: String },
    #[error("invalid capsulehub link: {0}")]
    InvalidLink(&'static str),
    #[error("invalid capsulehub key: {0}")]
    InvalidKey(&'static str),
}

/// 32-byte symmetric key for one hub object.
///
/// `Debug` and `Display` are redacted; the bytes never reach logs.
pub struct HubKey([u8; HUB_KEY_BYTES]);

impl HubKey {
    /// Generates a fresh random key.
    pub fn generate() -> Result<Self, HubError> {
        let mut bytes = [0u8; HUB_KEY_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Ok(Self(bytes))
    }

    /// Wraps 32 raw key bytes.
    pub fn from_bytes(bytes: [u8; HUB_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Encodes the key as unpadded base64url for the link fragment.
    pub fn to_fragment_secret(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Strictly parses an unpadded base64url fragment key.
    ///
    /// Rejects padded, non-canonical, and wrong-length values.
    pub fn from_fragment_secret(text: &str) -> Result<Self, HubError> {
        if text.len() != HUB_KEY_FRAGMENT_BYTES {
            return Err(HubError::InvalidKey(
                "key must be 32 bytes encoded as unpadded base64url",
            ));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| HubError::InvalidKey("key is not unpadded base64url"))?;
        let bytes: [u8; HUB_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| HubError::InvalidKey("key must decode to 32 bytes"))?;
        if URL_SAFE_NO_PAD.encode(bytes) != text {
            return Err(HubError::InvalidKey("key is not canonical base64url"));
        }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for HubKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HubKey(<redacted>)")
    }
}

impl fmt::Display for HubKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Content address of a capsulehub ciphertext object (`sha256:<64 hex>`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HubObjectId(String);

impl HubObjectId {
    /// Strictly parses `sha256:<64 lowercase hex>`.
    pub fn parse(text: &str) -> Result<Self, HubError> {
        if !is_sha256_id(text) {
            return Err(HubError::InvalidLink(
                "object id must be `sha256:<64 lowercase hex>`",
            ));
        }
        Ok(Self(text.to_owned()))
    }

    /// Returns the full `sha256:<hex>` text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_digest(digest: &[u8]) -> Self {
        Self(format!("{HUB_OBJECT_ID_PREFIX}{}", hex::encode(digest)))
    }
}

impl fmt::Display for HubObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Object id of an in-memory ciphertext object.
pub fn object_id_of_bytes(ciphertext: &[u8]) -> HubObjectId {
    HubObjectId::from_digest(&Sha256::digest(ciphertext))
}

/// Recomputes the object id of an existing ciphertext file.
pub fn object_id_of_file(path: &Path) -> Result<HubObjectId, HubError> {
    let mut reader = BufReader::with_capacity(HUB_CHUNK_BYTES, File::open(path)?);
    object_id_of_reader(&mut reader)
}

/// Streams constant-memory chunks, writes the ciphertext object to `output`,
/// and returns the object id of what was written.
pub fn encrypt_file(key: &HubKey, input: &Path, output: &Path) -> Result<HubObjectId, HubError> {
    let size = fs::metadata(input)?.len();
    if size > MAX_HUB_PLAINTEXT_BYTES {
        return Err(HubError::TooLarge);
    }
    let reader = BufReader::with_capacity(HUB_CHUNK_BYTES, File::open(input)?);
    let writer = BufWriter::with_capacity(HUB_CHUNK_BYTES, File::create(output)?);
    encrypt_stream(key, reader, writer)
}

/// Streaming decrypt.
///
/// Verifies `expected` against the hash of the ciphertext actually read before
/// writing any plaintext, then authenticates and decrypts every frame into
/// `output`. A rejected object may still leave a partial `output` behind; the
/// caller owns the destination and must remove it on error.
pub fn decrypt_file(
    key: &HubKey,
    expected: &HubObjectId,
    input: &Path,
    output: &Path,
) -> Result<(), HubError> {
    let actual = object_id_of_file(input)?;
    if &actual != expected {
        return Err(HubError::ObjectIdMismatch {
            expected: expected.as_str().to_owned(),
            actual: actual.as_str().to_owned(),
        });
    }
    let reader = BufReader::with_capacity(HUB_CHUNK_BYTES, File::open(input)?);
    let mut writer = BufWriter::with_capacity(HUB_CHUNK_BYTES, File::create(output)?);
    decrypt_stream(key, reader, &mut writer)
}

/// In-memory encryption for tests and small inputs.
pub fn encrypt_bytes(key: &HubKey, plaintext: &[u8]) -> Result<(HubObjectId, Vec<u8>), HubError> {
    if plaintext.len() as u64 > MAX_HUB_PLAINTEXT_BYTES {
        return Err(HubError::TooLarge);
    }
    let mut ciphertext = Vec::new();
    let object_id = encrypt_stream(key, plaintext, &mut ciphertext)?;
    Ok((object_id, ciphertext))
}

/// In-memory decryption for tests and small inputs.
pub fn decrypt_bytes(
    key: &HubKey,
    expected: &HubObjectId,
    ciphertext: &[u8],
) -> Result<Vec<u8>, HubError> {
    let actual = object_id_of_bytes(ciphertext);
    if &actual != expected {
        return Err(HubError::ObjectIdMismatch {
            expected: expected.as_str().to_owned(),
            actual: actual.as_str().to_owned(),
        });
    }
    let mut plaintext = Vec::new();
    decrypt_stream(key, ciphertext, &mut plaintext)?;
    Ok(plaintext)
}

/// A shared capsulehub link:
/// `https://host[/base]/c/#o=<object id>&k=<key>&cid=<contract ref>`.
///
/// The key is carried only in the fragment, never in the path or query.
pub struct SharedLink {
    pub object_id: HubObjectId,
    pub key: HubKey,
    /// Claimed capsule identity (`root_contract_ref`) recomputed on pull.
    pub cid: String,
    base: String,
}

impl SharedLink {
    /// Parses a shared link strictly.
    ///
    /// Rejects a non-http(s) scheme, userinfo, a query, a missing or wrong
    /// `/c/` path, and unknown, duplicate, or missing fragment parameters.
    pub fn parse(url: &str) -> Result<Self, HubError> {
        let (target, fragment) = url
            .split_once('#')
            .ok_or(HubError::InvalidLink("link has no fragment"))?;
        if target.contains('?') {
            return Err(HubError::InvalidLink("link must not carry a query"));
        }
        let (scheme, rest) = target.split_once("://").ok_or(HubError::InvalidLink(
            "link must be an absolute http(s) URL",
        ))?;
        if scheme != "http" && scheme != "https" {
            return Err(HubError::InvalidLink("link scheme must be http or https"));
        }
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, String::new()),
        };
        validate_authority(authority)?;
        let base_path = path
            .strip_suffix("/c/")
            .ok_or(HubError::InvalidLink("link path must end with `/c/`"))?;
        let base = format_base(scheme, authority, base_path);

        let mut object_id = None;
        let mut key = None;
        let mut cid = None;
        for parameter in fragment.split('&') {
            let (name, value) = parameter.split_once('=').ok_or(HubError::InvalidLink(
                "fragment parameters must use `name=value`",
            ))?;
            match name {
                "o" => {
                    if object_id.is_some() {
                        return Err(HubError::InvalidLink("duplicate `o` fragment parameter"));
                    }
                    object_id = Some(HubObjectId::parse(value)?);
                }
                "k" => {
                    if key.is_some() {
                        return Err(HubError::InvalidLink("duplicate `k` fragment parameter"));
                    }
                    key = Some(HubKey::from_fragment_secret(value)?);
                }
                "cid" => {
                    if cid.is_some() {
                        return Err(HubError::InvalidLink("duplicate `cid` fragment parameter"));
                    }
                    if !is_sha256_id(value) {
                        return Err(HubError::InvalidLink(
                            "`cid` must be `sha256:<64 lowercase hex>`",
                        ));
                    }
                    cid = Some(value.to_owned());
                }
                _ => return Err(HubError::InvalidLink("unknown fragment parameter")),
            }
        }
        let object_id =
            object_id.ok_or(HubError::InvalidLink("link is missing the `o` parameter"))?;
        let key = key.ok_or(HubError::InvalidLink("link is missing the `k` parameter"))?;
        let cid = cid.ok_or(HubError::InvalidLink("link is missing the `cid` parameter"))?;

        Ok(Self {
            object_id,
            key,
            cid,
            base,
        })
    }

    /// Builds a link rooted at `hub_base` (an origin plus optional path base,
    /// without the `/c/` segment).
    pub fn new(
        hub_base: &str,
        object_id: HubObjectId,
        key: HubKey,
        cid: impl Into<String>,
    ) -> Result<Self, HubError> {
        let base = normalize_hub_base(hub_base)?;
        let cid = cid.into();
        if !is_sha256_id(&cid) {
            return Err(HubError::InvalidLink(
                "`cid` must be `sha256:<64 lowercase hex>`",
            ));
        }
        Ok(Self {
            object_id,
            key,
            cid,
            base,
        })
    }

    /// Renders the link against `hub_base`.
    pub fn to_url(&self, hub_base: &str) -> Result<String, HubError> {
        let base = normalize_hub_base(hub_base)?;
        Ok(format!(
            "{base}/c/#o={}&k={}&cid={}",
            self.object_id.as_str(),
            self.key.to_fragment_secret(),
            self.cid
        ))
    }

    /// Link origin plus base path, with the trailing `/c/` removed.
    pub fn base_url(&self) -> Result<String, HubError> {
        Ok(self.base.clone())
    }
}

impl fmt::Debug for SharedLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedLink")
            .field("object_id", &self.object_id)
            .field("key", &"<redacted>")
            .field("cid", &self.cid)
            .field("base", &self.base)
            .finish()
    }
}

/// Encrypts `input` into `output` with wire v1, returning the ciphertext id.
fn encrypt_stream<R: Read, W: Write>(
    key: &HubKey,
    mut input: R,
    mut output: W,
) -> Result<HubObjectId, HubError> {
    let (header, mut push) = PushStream::init(OsRng, &Key::from(key.0));
    let header_bytes: [u8; HUB_SECRETSTREAM_HEADER_BYTES] = *header.as_ref();
    let associated_data = frame_associated_data(&header_bytes);
    let mut hasher = Sha256::new();
    write_hashed(&mut output, &mut hasher, &HUB_MAGIC)?;
    write_hashed(&mut output, &mut hasher, &[HUB_WIRE_VERSION])?;
    write_hashed(&mut output, &mut hasher, &header_bytes)?;

    let mut total = 0u64;
    let mut chunk = read_chunk(&mut input)?;
    loop {
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_HUB_PLAINTEXT_BYTES {
            return Err(HubError::TooLarge);
        }
        let next = read_chunk(&mut input)?;
        let tag = if next.is_empty() {
            Tag::Final
        } else {
            Tag::Message
        };
        let mut body = chunk;
        push.push(&mut body, &associated_data, tag)
            .map_err(|_| HubError::InvalidFrame("secretstream encryption failed"))?;
        write_frame(&mut output, &mut hasher, &body)?;
        if tag == Tag::Final {
            break;
        }
        chunk = next;
    }
    output.flush()?;
    Ok(HubObjectId::from_digest(&hasher.finalize()))
}

/// Decrypts a wire v1 stream, writing plaintext chunks to `output`.
fn decrypt_stream<R: Read, W: Write>(
    key: &HubKey,
    mut input: R,
    mut output: W,
) -> Result<(), HubError> {
    let mut prefix = [0u8; HUB_PREFIX_BYTES];
    read_exact_or_truncated(&mut input, &mut prefix)?;
    if prefix[..HUB_MAGIC.len()] != HUB_MAGIC {
        return Err(HubError::BadMagic);
    }
    let version = prefix[HUB_MAGIC.len()];
    if version != HUB_WIRE_VERSION {
        return Err(HubError::UnsupportedVersion(version));
    }
    let mut header_bytes = [0u8; HUB_SECRETSTREAM_HEADER_BYTES];
    header_bytes.copy_from_slice(&prefix[HUB_MAGIC.len() + 1..]);
    let associated_data = frame_associated_data(&header_bytes);
    let mut pull = PullStream::init(Header::from(header_bytes), &Key::from(key.0));

    let mut total = 0u64;
    loop {
        let Some(body_len) = read_frame_length(&mut input)? else {
            return Err(HubError::MissingFinal);
        };
        let body_len = usize::try_from(body_len)
            .map_err(|_| HubError::InvalidFrame("frame body length is out of range"))?;
        if !(HUB_MIN_FRAME_BODY_BYTES..=HUB_MAX_FRAME_BODY_BYTES).contains(&body_len) {
            return Err(HubError::InvalidFrame("frame body length is out of range"));
        }
        let mut body = vec![0u8; body_len];
        read_exact_or_truncated(&mut input, &mut body)?;
        let tag = pull
            .pull(&mut body, &associated_data)
            .map_err(|_| HubError::Authentication)?;
        total = total.saturating_add(body.len() as u64);
        if total > MAX_HUB_PLAINTEXT_BYTES {
            return Err(HubError::TooLarge);
        }
        match tag {
            Tag::Message => output.write_all(&body)?,
            Tag::Final => {
                output.write_all(&body)?;
                output.flush()?;
                return ensure_end_of_input(&mut input);
            }
            Tag::Push | Tag::Rekey => {
                return Err(HubError::InvalidFrame(
                    "only message and final tags are allowed",
                ));
            }
        }
    }
}

fn object_id_of_reader<R: Read>(reader: &mut R) -> Result<HubObjectId, HubError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HUB_CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(HubObjectId::from_digest(&hasher.finalize()))
}

/// Reads at most one chunk, filling it completely unless the input ends.
fn read_chunk<R: Read>(reader: &mut R) -> Result<Vec<u8>, HubError> {
    let mut buffer = vec![0u8; HUB_CHUNK_BYTES];
    let mut filled = 0;
    while filled < HUB_CHUNK_BYTES {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

fn read_frame_length<R: Read>(reader: &mut R) -> Result<Option<u32>, HubError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(HubError::Truncated),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(u32::from_le_bytes(header)))
}

fn read_exact_or_truncated<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<(), HubError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Err(HubError::Truncated),
        Err(error) => Err(error.into()),
    }
}

/// After a final frame, the object must end immediately.
fn ensure_end_of_input<R: Read>(reader: &mut R) -> Result<(), HubError> {
    let mut probe = [0u8; 1];
    loop {
        match reader.read(&mut probe) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(HubError::TrailingData),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_frame<W: Write>(output: &mut W, hasher: &mut Sha256, body: &[u8]) -> Result<(), HubError> {
    let len =
        u32::try_from(body.len()).map_err(|_| HubError::InvalidFrame("frame body is too large"))?;
    write_hashed(output, hasher, &len.to_le_bytes())?;
    write_hashed(output, hasher, body)
}

fn write_hashed<W: Write>(
    output: &mut W,
    hasher: &mut Sha256,
    bytes: &[u8],
) -> Result<(), HubError> {
    output.write_all(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn frame_associated_data(header: &[u8; HUB_SECRETSTREAM_HEADER_BYTES]) -> [u8; HUB_PREFIX_BYTES] {
    let mut associated_data = [0u8; HUB_PREFIX_BYTES];
    associated_data[..HUB_MAGIC.len()].copy_from_slice(&HUB_MAGIC);
    associated_data[HUB_MAGIC.len()] = HUB_WIRE_VERSION;
    associated_data[HUB_MAGIC.len() + 1..].copy_from_slice(header);
    associated_data
}

/// `sha256:` followed by exactly 64 lowercase hex characters.
fn is_sha256_id(text: &str) -> bool {
    let Some(hex) = text.strip_prefix(HUB_OBJECT_ID_PREFIX) else {
        return false;
    };
    hex.len() == HUB_SHA256_HEX_LEN
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Escapes nothing: a link must be usable as-is in an HTTP request target.
fn validate_authority(authority: &str) -> Result<(), HubError> {
    if authority.is_empty() {
        return Err(HubError::InvalidLink("link has no host"));
    }
    if authority.contains('@') {
        return Err(HubError::InvalidLink("link must not carry userinfo"));
    }
    if !authority.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']' | b'_')
    }) {
        return Err(HubError::InvalidLink(
            "link host contains unsupported characters",
        ));
    }
    Ok(())
}

fn format_base(scheme: &str, authority: &str, base_path: &str) -> String {
    let base_path = base_path.trim_end_matches('/');
    format!("{scheme}://{authority}{base_path}")
}

/// Normalizes and validates a hub base URL (origin plus optional path base).
fn normalize_hub_base(hub_base: &str) -> Result<String, HubError> {
    if hub_base.contains('#') {
        return Err(HubError::InvalidLink("hub base must not carry a fragment"));
    }
    if hub_base.contains('?') {
        return Err(HubError::InvalidLink("hub base must not carry a query"));
    }
    let (scheme, rest) = hub_base.split_once("://").ok_or(HubError::InvalidLink(
        "hub base must be an absolute http(s) URL",
    ))?;
    if scheme != "http" && scheme != "https" {
        return Err(HubError::InvalidLink(
            "hub base scheme must be http or https",
        ));
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, String::new()),
    };
    validate_authority(authority)?;
    let path = path.trim_end_matches('/');
    if path == "/c" || path.ends_with("/c") {
        return Err(HubError::InvalidLink(
            "hub base must not include the `/c/` segment",
        ));
    }
    Ok(format!("{scheme}://{authority}{path}"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Bound from the design: one read/write must stay within chunk + 37.
    const MAX_SINGLE_IO_BYTES: usize = HUB_CHUNK_BYTES + 37;

    fn key() -> HubKey {
        HubKey::from_bytes([0x42; HUB_KEY_BYTES])
    }

    fn other_key() -> HubKey {
        HubKey::from_bytes([0x24; HUB_KEY_BYTES])
    }

    fn sha256_id(bytes: &[u8]) -> HubObjectId {
        object_id_of_bytes(bytes)
    }

    fn decrypt_tampered(ciphertext: &[u8]) -> Result<Vec<u8>, HubError> {
        decrypt_bytes(&key(), &sha256_id(ciphertext), ciphertext)
    }

    fn raw_ciphertext(key: &HubKey, frames: &[(&[u8], Tag)]) -> Vec<u8> {
        let (header, mut push) = PushStream::init(OsRng, &Key::from(key.0));
        let header_bytes: [u8; HUB_SECRETSTREAM_HEADER_BYTES] = *header.as_ref();
        let associated_data = frame_associated_data(&header_bytes);
        let mut ciphertext = Vec::new();
        ciphertext.extend_from_slice(&HUB_MAGIC);
        ciphertext.push(HUB_WIRE_VERSION);
        ciphertext.extend_from_slice(&header_bytes);
        for (plaintext, tag) in frames {
            let mut body = plaintext.to_vec();
            push.push(&mut body, &associated_data, *tag).unwrap();
            ciphertext.extend_from_slice(&(body.len() as u32).to_le_bytes());
            ciphertext.extend_from_slice(&body);
        }
        ciphertext
    }

    fn frame_len_at(ciphertext: &[u8], offset: usize) -> usize {
        u32::from_le_bytes(ciphertext[offset..offset + 4].try_into().unwrap()) as usize
    }

    #[test]
    fn round_trips_empty_one_and_many_chunks() {
        for plaintext in [
            Vec::new(),
            b"small payload".to_vec(),
            vec![0x5a; HUB_CHUNK_BYTES],
            vec![0xa5; HUB_CHUNK_BYTES + 5],
            vec![0x11; 2 * HUB_CHUNK_BYTES + 100],
        ] {
            let (object_id, ciphertext) = encrypt_bytes(&key(), &plaintext).unwrap();
            assert_eq!(
                decrypt_bytes(&key(), &object_id, &ciphertext).unwrap(),
                plaintext
            );
            assert_eq!(object_id, sha256_id(&ciphertext));
        }
    }

    #[test]
    fn empty_plaintext_is_a_single_empty_final_frame() {
        let (_, ciphertext) = encrypt_bytes(&key(), b"").unwrap();
        assert_eq!(ciphertext.len(), HUB_PREFIX_BYTES + 4 + 17);
    }

    #[test]
    fn frame_count_and_lengths_match_the_chunk_rule() {
        let plaintext = vec![1u8; 2 * HUB_CHUNK_BYTES + 9];
        let (_, ciphertext) = encrypt_bytes(&key(), &plaintext).unwrap();
        let mut offset = HUB_PREFIX_BYTES;
        let mut lengths = Vec::new();
        while offset < ciphertext.len() {
            let body_len = frame_len_at(&ciphertext, offset);
            lengths.push(body_len);
            offset += 4 + body_len;
        }
        assert_eq!(offset, ciphertext.len());
        assert_eq!(
            lengths,
            vec![HUB_CHUNK_BYTES + 17, HUB_CHUNK_BYTES + 17, 9 + 17,]
        );
    }

    #[test]
    fn rejects_a_wrong_key() {
        let (object_id, ciphertext) = encrypt_bytes(&key(), b"secret").unwrap();
        assert!(matches!(
            decrypt_bytes(&other_key(), &object_id, &ciphertext),
            Err(HubError::Authentication)
        ));
    }

    #[test]
    fn rejects_a_bad_magic() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext[0] ^= 0x01;
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::BadMagic)
        ));
    }

    #[test]
    fn rejects_an_unsupported_version() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext[6] = 9;
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::UnsupportedVersion(9))
        ));
    }

    #[test]
    fn rejects_a_tampered_header() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext[7] ^= 0xff;
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::Authentication)
        ));
    }

    #[test]
    fn rejects_a_tampered_frame_body() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x80;
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::Authentication)
        ));
    }

    #[test]
    fn rejects_an_out_of_range_frame_length() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext[31..35].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::InvalidFrame(_))
        ));

        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        let too_large = (HUB_CHUNK_BYTES + 17 + 1) as u32;
        ciphertext[31..35].copy_from_slice(&too_large.to_le_bytes());
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::InvalidFrame(_))
        ));
    }

    #[test]
    fn rejects_a_shortened_frame_length() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        let body_len = frame_len_at(&ciphertext, HUB_PREFIX_BYTES) as u32;
        ciphertext[31..35].copy_from_slice(&(body_len - 1).to_le_bytes());
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::Authentication | HubError::Truncated | HubError::MissingFinal)
        ));
    }

    #[test]
    fn rejects_truncation_at_several_offsets() {
        let (_, ciphertext) = encrypt_bytes(&key(), b"payload that spans one frame").unwrap();
        for offset in [
            0,
            1,
            HUB_MAGIC.len(),
            HUB_MAGIC.len() + 1,
            20,
            HUB_PREFIX_BYTES - 1,
            HUB_PREFIX_BYTES,
            HUB_PREFIX_BYTES + 1,
            HUB_PREFIX_BYTES + 4,
            HUB_PREFIX_BYTES + 5,
            ciphertext.len() - 1,
        ] {
            assert!(
                decrypt_tampered(&ciphertext[..offset]).is_err(),
                "truncation at {offset} must fail"
            );
        }
    }

    #[test]
    fn rejects_swapped_frames() {
        let first = vec![0x01; 64];
        let second = vec![0x02; 64];
        let mut ciphertext = raw_ciphertext(
            &key(),
            &[
                (&first, Tag::Message),
                (&second, Tag::Message),
                (b"end", Tag::Final),
            ],
        );
        let start = HUB_PREFIX_BYTES + 4;
        let first_body: Vec<u8> = ciphertext[start..start + first.len() + 17].to_vec();
        let second_start = start + first.len() + 17 + 4;
        let second_body: Vec<u8> =
            ciphertext[second_start..second_start + second.len() + 17].to_vec();
        ciphertext[start..start + first_body.len()].copy_from_slice(&second_body);
        ciphertext[second_start..second_start + second_body.len()].copy_from_slice(&first_body);
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::Authentication)
        ));
    }

    #[test]
    fn rejects_a_message_frame_in_final_position() {
        let ciphertext = raw_ciphertext(&key(), &[(b"payload", Tag::Message)]);
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::MissingFinal)
        ));
    }

    #[test]
    fn rejects_a_duplicated_final_frame() {
        let ciphertext = raw_ciphertext(
            &key(),
            &[(b"a", Tag::Message), (b"b", Tag::Final), (b"c", Tag::Final)],
        );
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::TrailingData)
        ));
    }

    #[test]
    fn rejects_push_and_rekey_tags() {
        for tag in [Tag::Push, Tag::Rekey] {
            let ciphertext = raw_ciphertext(&key(), &[(b"payload", tag)]);
            assert!(matches!(
                decrypt_tampered(&ciphertext),
                Err(HubError::InvalidFrame(_))
            ));
        }
    }

    #[test]
    fn rejects_trailing_bytes_after_the_final_frame() {
        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext.push(b'x');
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::TrailingData)
        ));

        let (_, mut ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        ciphertext.extend_from_slice(&[0u8; 8]);
        assert!(matches!(
            decrypt_tampered(&ciphertext),
            Err(HubError::TrailingData)
        ));
    }

    #[test]
    fn reports_object_id_mismatch_before_any_decryption() {
        let (_, ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        let wrong = sha256_id(b"different bytes");
        match decrypt_bytes(&key(), &wrong, &ciphertext) {
            Err(HubError::ObjectIdMismatch { expected, actual }) => {
                assert_eq!(expected, wrong.as_str());
                assert_eq!(actual, sha256_id(&ciphertext).as_str());
            }
            other => panic!("expected object id mismatch, got {other:?}"),
        }
    }

    #[test]
    fn file_round_trip_and_ciphertext_size() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("bundle.capsule");
        let encrypted = directory.path().join("bundle.hub");
        let decrypted = directory.path().join("bundle.out");
        let plaintext = vec![0x33; 2 * HUB_CHUNK_BYTES + 123];
        fs::write(&input, &plaintext).unwrap();

        let object_id = encrypt_file(&key(), &input, &encrypted).unwrap();
        assert_eq!(fs::metadata(&encrypted).unwrap().len() as usize, {
            let frame = 4 + 17;
            HUB_PREFIX_BYTES + plaintext.len() + 3 * frame
        });
        assert_eq!(object_id_of_file(&encrypted).unwrap(), object_id);

        decrypt_file(&key(), &object_id, &encrypted, &decrypted).unwrap();
        assert_eq!(fs::read(&decrypted).unwrap(), plaintext);
    }

    #[test]
    fn decrypt_file_checks_the_object_id_before_writing_plaintext() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("bundle.hub");
        let output = directory.path().join("out");
        let (object_id, ciphertext) = encrypt_bytes(&key(), b"payload").unwrap();
        fs::write(&input, &ciphertext).unwrap();

        let wrong = sha256_id(b"different bytes");
        assert!(matches!(
            decrypt_file(&key(), &wrong, &input, &output),
            Err(HubError::ObjectIdMismatch { .. })
        ));
        assert!(!output.exists());
        assert!(decrypt_file(&key(), &object_id, &input, &output).is_ok());
    }

    struct BoundedReader<R> {
        inner: R,
    }

    impl<R: Read> Read for BoundedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(
                buffer.len() <= MAX_SINGLE_IO_BYTES,
                "single read of {} bytes exceeds the streaming bound of {}",
                buffer.len(),
                MAX_SINGLE_IO_BYTES
            );
            self.inner.read(buffer)
        }
    }

    struct BoundedWriter<W> {
        inner: W,
    }

    impl<W: Write> Write for BoundedWriter<W> {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            assert!(
                buffer.len() <= MAX_SINGLE_IO_BYTES,
                "single write of {} bytes exceeds the streaming bound of {}",
                buffer.len(),
                MAX_SINGLE_IO_BYTES
            );
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn streaming_encrypt_and_decrypt_stay_within_the_chunk_bound() {
        // Three chunks: one short of the multi-chunk, plus a partial tail.
        let plaintext = vec![0x7f; 2 * HUB_CHUNK_BYTES + 4096];
        let mut ciphertext = Vec::new();
        let object_id = encrypt_stream(
            &key(),
            BoundedReader {
                inner: Cursor::new(&plaintext),
            },
            BoundedWriter {
                inner: &mut ciphertext,
            },
        )
        .unwrap();

        let mut decrypted = Vec::new();
        decrypt_stream(
            &key(),
            BoundedReader {
                inner: Cursor::new(&ciphertext),
            },
            BoundedWriter {
                inner: &mut decrypted,
            },
        )
        .unwrap();

        assert_eq!(decrypted, plaintext);
        assert_eq!(object_id, sha256_id(&ciphertext));
    }

    #[test]
    fn generated_keys_round_trip_through_the_fragment_secret() {
        let generated = HubKey::generate().unwrap();
        let secret = generated.to_fragment_secret();
        assert_eq!(secret.len(), HUB_KEY_FRAGMENT_BYTES);
        let parsed = HubKey::from_fragment_secret(&secret).unwrap();
        assert_eq!(parsed.to_fragment_secret(), secret);
        assert_ne!(
            HubKey::generate().unwrap().to_fragment_secret(),
            secret,
            "generated keys must differ"
        );
    }

    #[test]
    fn key_types_never_reveal_material_in_debug_or_display() {
        let key = key();
        let secret = key.to_fragment_secret();
        for rendered in [format!("{key:?}"), format!("{key}")] {
            assert!(!rendered.contains(&secret));
            assert!(rendered.contains("redacted"));
        }
        let link = link();
        let link_debug = format!("{link:?}");
        assert!(!link_debug.contains(&secret));
        assert!(link_debug.contains("<redacted>"));
    }

    fn object_hex() -> String {
        "a".repeat(64)
    }

    fn cid_hex() -> String {
        "b".repeat(64)
    }

    fn link() -> SharedLink {
        SharedLink::new(
            "https://hub.example",
            HubObjectId::parse(&format!("sha256:{}", object_hex())).unwrap(),
            key(),
            format!("sha256:{}", cid_hex()),
        )
        .unwrap()
    }

    #[test]
    fn parses_a_well_formed_link_and_round_trips_it() {
        let link = link();
        let url = link.to_url("https://hub.example").unwrap();
        assert_eq!(
            url,
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                key().to_fragment_secret(),
                cid_hex()
            )
        );
        let parsed = SharedLink::parse(&url).unwrap();
        assert_eq!(parsed.object_id, link.object_id);
        assert_eq!(
            parsed.key.to_fragment_secret(),
            link.key.to_fragment_secret()
        );
        assert_eq!(parsed.cid, link.cid);
        assert_eq!(parsed.base_url().unwrap(), "https://hub.example");
        assert_eq!(parsed.to_url("https://hub.example").unwrap(), url);
    }

    #[test]
    fn parses_a_link_with_a_base_path_and_parameter_permutations() {
        let url = format!(
            "https://hub.example/team/c/#cid=sha256:{}&o=sha256:{}&k={}",
            cid_hex(),
            object_hex(),
            key().to_fragment_secret()
        );
        let parsed = SharedLink::parse(&url).unwrap();
        assert_eq!(parsed.base_url().unwrap(), "https://hub.example/team");
        assert_eq!(parsed.to_url("https://hub.example/team").unwrap(), {
            format!(
                "https://hub.example/team/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                key().to_fragment_secret(),
                cid_hex()
            )
        });
    }

    #[test]
    fn accepts_http_and_loopback_forms() {
        let key_text = key().to_fragment_secret();
        for url in [
            format!(
                "http://127.0.0.1:8080/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                key_text,
                cid_hex()
            ),
            format!(
                "https://[::1]:8443/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                key_text,
                cid_hex()
            ),
            format!(
                "https://hub.example/base/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                key_text,
                cid_hex()
            ),
        ] {
            assert!(SharedLink::parse(&url).is_ok(), "must parse {url}");
        }
    }

    #[test]
    fn rejects_links_with_the_wrong_shape() {
        let key_text = key().to_fragment_secret();
        let fragment = format!(
            "o=sha256:{}&k={}&cid=sha256:{}",
            object_hex(),
            key_text,
            cid_hex()
        );
        let cases = [
            "https://hub.example/c/".to_owned(),
            "https://hub.example/c/#".to_owned(),
            format!("https://hub.example/#{fragment}"),
            format!("https://hub.example/x/#{fragment}"),
            format!("https://hub.example/c/{fragment}"),
            format!("https://hub.example/c/?x=1#{fragment}"),
            format!("https://hub.example/c/#{fragment}&x=1"),
            format!(
                "https://hub.example/c/#{fragment}&o=sha256:{}",
                object_hex()
            ),
            format!("https://user:pw@hub.example/c/#{fragment}"),
            format!("ftp://hub.example/c/#{fragment}"),
            format!("file://hub.example/c/#{fragment}"),
            format!("//hub.example/c/#{fragment}"),
            format!("hub.example/c/#{fragment}"),
            format!(
                "https://hub.example/c/#o=sha256:{}&cid=sha256:{}",
                object_hex(),
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}",
                object_hex(),
                key_text
            ),
            format!(
                "https://hub.example/c/#k={}&cid=sha256:{}",
                key_text,
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}",
                object_hex(),
                key_text
            ),
            format!(
                "https://hub.example/c/#o=not-a-digest&k={}&cid=sha256:{}",
                key_text,
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}&cid=blake3:{}",
                object_hex(),
                key_text,
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}&cid=sha256:{}&cid=sha256:{}",
                object_hex(),
                key_text,
                cid_hex(),
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}&cid=sha256:{}",
                object_hex(),
                "not-a-key",
                cid_hex()
            ),
            format!(
                "https://hub.example/c/#o=sha256:{}&k={}=&cid=sha256:{}",
                object_hex(),
                key_text,
                cid_hex()
            ),
            format!("https://hub.example/c/#=value&o=sha256:{}", object_hex()),
            format!("https://hub.example/c/##{fragment}"),
        ];
        for url in cases {
            assert!(
                SharedLink::parse(&url).is_err(),
                "link must be rejected: {url}"
            );
        }
    }

    #[test]
    fn rejects_non_canonical_key_encodings() {
        let padded = format!("{}=", key().to_fragment_secret());
        assert!(matches!(
            HubKey::from_fragment_secret(&padded),
            Err(HubError::InvalidKey(_))
        ));
        assert!(matches!(
            HubKey::from_fragment_secret(""),
            Err(HubError::InvalidKey(_))
        ));
        assert!(matches!(
            HubKey::from_fragment_secret(&"!".repeat(HUB_KEY_FRAGMENT_BYTES)),
            Err(HubError::InvalidKey(_))
        ));
    }

    #[test]
    fn rejects_hub_bases_that_already_contain_the_c_segment_or_a_query() {
        let link = link();
        for base in [
            "https://hub.example/c",
            "https://hub.example/c/",
            "https://hub.example?x=1",
            "https://hub.example#frag",
            "ftp://hub.example",
            "https://user@hub.example",
            "hub.example",
        ] {
            assert!(
                link.to_url(base).is_err(),
                "hub base must be rejected: {base}"
            );
        }
        assert_eq!(
            link.to_url("https://hub.example/team/").unwrap(),
            link.to_url("https://hub.example/team").unwrap()
        );
    }

    #[test]
    fn rejects_links_carrying_percent_encoded_or_uppercase_ids() {
        let key_text = key().to_fragment_secret();
        for value in [
            "SHA256:".to_owned() + &object_hex(),
            "sha256:".to_owned() + &"A".repeat(64),
            "sha256:".to_owned() + &"a".repeat(63),
            "sha256:".to_owned() + &"a".repeat(65),
        ] {
            assert!(HubObjectId::parse(&value).is_err());
            let url = format!(
                "https://hub.example/c/#o={}&k={}&cid=sha256:{}",
                value,
                key_text,
                cid_hex()
            );
            assert!(SharedLink::parse(&url).is_err());
        }
    }

    #[test]
    fn shared_link_construction_validates_the_cid_claim() {
        let object = HubObjectId::parse(&format!("sha256:{}", object_hex())).unwrap();
        assert!(matches!(
            SharedLink::new("https://hub.example", object, key(), "not-a-cid"),
            Err(HubError::InvalidLink(_))
        ));
    }
}
