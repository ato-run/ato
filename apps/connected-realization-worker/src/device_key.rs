//! Runner-local Ed25519 device key (RFC SELF_HOST_RUNNER_NETWORK_V1 section 11).
//!
//! The key is generated on this host at enrollment, stored next to — never
//! inside — the bearer credential file with mode 0600, and used only to sign
//! control requests. The private key never leaves this host and is never
//! printed, logged, or embedded in an error message: Debug redacts it and every
//! error this module produces talks about the file, never its contents.
//!
//! The wire strings here are the canonical signing strings from section 11.3.
//! They are newline separated with NO trailing newline and the fields in the
//! exact documented order; see control_signing_string and friends.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(all(test, unix))]
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::write_private_file;

/// Name of the key file kept beside the bearer credential file.
pub const DEVICE_KEY_FILE_NAME: &str = "runner-device-key.json";

/// Algorithm recorded in the key file. Section 11.2 fixes ed25519.
pub const DEVICE_KEY_ALGORITHM: &str = "ed25519";
/// Section 11.2: the only accepted signature freshness window. The Runner only
/// witnesses it (the stub verifiers do); the control plane enforces it.
#[cfg(all(test, unix))]
pub const SIGNATURE_FRESHNESS: Duration = Duration::from_secs(300);
/// Section 11.2: key epochs start at 1 and strictly increase per Runner.
pub const FIRST_KEY_EPOCH: u64 = 1;

/// Whether this platform can enforce the owner-only file mode the contract
/// requires (section 11.8).
///
/// Only Unix can: the mode is set at creation and checked again on every load.
/// Where that cannot be guaranteed this Runner does not hold a device key at
/// all and keeps the bearer-only behaviour instead. That is an explicit
/// refusal, never an assumption that the platform is safe.
pub const fn supported() -> bool {
    cfg!(unix)
}

fn ensure_supported() -> Result<()> {
    ensure!(
        supported(),
        "device keys need Unix owner-only file permissions (0600), which this platform cannot enforce; refusing to create one"
    );
    Ok(())
}

/// Section 11.4 header carrying the epoch the signature was made with.
pub const KEY_EPOCH_HEADER: &str = "X-Ato-Runner-Key-Epoch";
/// Section 11.4 header carrying the RFC 3339 UTC second-precision timestamp.
pub const TIMESTAMP_HEADER: &str = "X-Ato-Runner-Timestamp";
/// Section 11.4 header carrying the base64url single-use nonce.
pub const NONCE_HEADER: &str = "X-Ato-Runner-Nonce";
/// Section 11.4 header carrying the base64url Ed25519 signature.
pub const SIGNATURE_HEADER: &str = "X-Ato-Runner-Signature";

const ENROLL_DOMAIN: &str = "ato-runner-enroll-v1";
const REGISTER_DOMAIN: &str = "ato-runner-key-v1";
const CONTROL_DOMAIN: &str = "ato-runner-control-v1";
const ROTATE_DOMAIN: &str = "ato-runner-rotate-v1";

const DEVICE_KEY_VERSION: u8 = 1;

/// On-disk shape of runner-device-key.json.
///
/// Field names are snake_case like the existing credential file. public_key is
/// redundant with secret_key on purpose: a mismatch is corruption, not a key to
/// trust, so loading fails closed instead of deriving a key the control plane
/// never registered.
#[derive(Serialize, Deserialize)]
struct StoredDeviceKey {
    version: u8,
    algorithm: String,
    #[serde(default)]
    runner_id: String,
    key_epoch: u64,
    public_key: String,
    secret_key: String,
    #[serde(default)]
    registered: bool,
    created_at: String,
}

/// This Runner's Ed25519 device key.
#[derive(Clone)]
pub struct RunnerDeviceKey {
    runner_id: String,
    key_epoch: u64,
    /// Whether the control plane has this key active. A key that was only
    /// generated locally is not used for signing: until it is registered the
    /// Runner keeps the bearer-only behaviour section 11.1 promises.
    registered: bool,
    created_at: String,
    signing: SigningKey,
}

impl fmt::Debug for RunnerDeviceKey {
    /// Public key material and bookkeeping only. The private key is redacted so
    /// no Debug, tracing field, or anyhow context can leak it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunnerDeviceKey")
            .field("runner_id", &self.runner_id)
            .field("key_epoch", &self.key_epoch)
            .field("registered", &self.registered)
            .field("public_key", &self.public_key_b64())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl PartialEq for RunnerDeviceKey {
    fn eq(&self, other: &Self) -> bool {
        self.runner_id == other.runner_id
            && self.key_epoch == other.key_epoch
            && self.registered == other.registered
            && self.public_key_bytes() == other.public_key_bytes()
    }
}

impl Eq for RunnerDeviceKey {}

impl RunnerDeviceKey {
    /// Generate a fresh key for this Runner at key_epoch.
    pub fn generate(runner_id: &str, key_epoch: u64) -> Result<Self> {
        ensure!(key_epoch >= FIRST_KEY_EPOCH, "device key epoch starts at 1");
        let mut seed = [0u8; 32];
        // The OS CSPRNG, not a userspace-seeded PRNG: this is a signing key.
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(Self {
            runner_id: runner_id.to_owned(),
            key_epoch,
            registered: false,
            created_at: now_timestamp(),
            signing,
        })
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    pub fn key_epoch(&self) -> u64 {
        self.key_epoch
    }

    pub fn registered(&self) -> bool {
        self.registered
    }

    pub fn set_registered(&mut self, registered: bool) {
        self.registered = registered;
    }

    pub fn set_runner_id(&mut self, runner_id: &str) {
        runner_id.clone_into(&mut self.runner_id);
    }

    /// Raw 32-byte public key; the wire form is base64url without padding.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn public_key_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public_key_bytes())
    }

    /// Verify a signature made over message by this key's public half. Used by
    /// the stub control plane in tests; never by the Runner in production.
    #[cfg(all(test, unix))]
    pub fn verifies(&self, message: &[u8], signature_b64: &str) -> bool {
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature_b64) else {
            return false;
        };
        let Ok(signature) = ed25519_dalek::Signature::from_slice(&signature) else {
            return false;
        };
        self.signing
            .verifying_key()
            .verify_strict(message, &signature)
            .is_ok()
    }

    /// The public half of this key, for a server-side verifier in tests.
    #[cfg(all(test, unix))]
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.signing.verifying_key()
    }

    fn sign(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.sign(message).to_bytes())
    }

    /// Section 11.4 signature over a control request. body is the exact request
    /// body bytes (empty when absent) and path_and_query starts with /.
    pub fn sign_control(
        &self,
        method: &str,
        path_and_query: &str,
        body: &[u8],
    ) -> SignedControlHeaders {
        self.sign_control_at(
            method,
            path_and_query,
            body,
            &now_timestamp(),
            &fresh_nonce(),
        )
    }

    /// Deterministic form for tests: the caller supplies the timestamp and nonce
    /// so staleness and replay cases can be exercised exactly.
    pub fn sign_control_at(
        &self,
        method: &str,
        path_and_query: &str,
        body: &[u8],
        timestamp: &str,
        nonce: &str,
    ) -> SignedControlHeaders {
        let message = control_signing_string(
            method,
            path_and_query,
            body,
            timestamp,
            nonce,
            self.key_epoch,
        );
        SignedControlHeaders {
            key_epoch: self.key_epoch,
            timestamp: timestamp.to_owned(),
            nonce: nonce.to_owned(),
            signature: self.sign(message.as_bytes()),
        }
    }

    /// Signature for the enroll string, made by the key the enrollment binds.
    /// Takes the enrollment token, whose SHA-256 (not the token) is signed. The
    /// token is single use, so it is itself the replay guard: no challenge is
    /// involved in enrollment.
    pub fn enrollment_signature(&self, enrollment_token: &str) -> String {
        self.sign(enrollment_signing_string(enrollment_token, self.key_epoch).as_bytes())
    }

    /// Signature for the register string (section 11.3).
    pub fn register_signature(&self, runner_id: &str, challenge_nonce: &str) -> String {
        self.sign(register_signing_string(runner_id, challenge_nonce, self.key_epoch).as_bytes())
    }

    /// Signature for the rotate string (section 11.3), made by THIS (current)
    /// key over the successor's epoch and public key.
    pub fn rotate_signature(
        &self,
        runner_id: &str,
        next: &RunnerDeviceKey,
        challenge_nonce: &str,
    ) -> String {
        self.sign(
            rotate_signing_string(
                runner_id,
                self.key_epoch,
                next.key_epoch,
                &next.public_key_bytes(),
                challenge_nonce,
            )
            .as_bytes(),
        )
    }

    /// Read the key file. Ok(None) means this Runner has no device key.
    ///
    /// Refuses a file other users can read: a key file with looser mode may
    /// already be compromised, and silently using it would hide that.
    pub fn load_if_present(path: &Path) -> Result<Option<Self>> {
        if !supported() {
            // No file is the ordinary bearer-only Runner. A file that exists is
            // a private key whose permissions this platform cannot check, so it
            // is refused rather than trusted.
            return if path.exists() {
                bail!(
                    "device key {} exists, but this platform cannot enforce the owner-only (0600) mode the contract requires; refusing to use it",
                    path.display()
                )
            } else {
                Ok(None)
            };
        }
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(path)
                .with_context(|| format!("cannot stat {}", path.display()))?
                .permissions()
                .mode()
                & 0o777;
            ensure!(
                mode & 0o077 == 0,
                "device key {} must be owner-only (0600) but is {:03o}; refusing to use it",
                path.display(),
                mode
            );
        }
        let stored: StoredDeviceKey = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a device key file", path.display()))?;
        Ok(Some(Self::from_stored(stored, path)?))
    }

    fn from_stored(stored: StoredDeviceKey, path: &Path) -> Result<Self> {
        ensure!(
            stored.version == DEVICE_KEY_VERSION,
            "device key {} has an unsupported version",
            path.display()
        );
        ensure!(
            stored.algorithm == DEVICE_KEY_ALGORITHM,
            "device key {} is not an Ed25519 key",
            path.display()
        );
        ensure!(
            stored.key_epoch >= FIRST_KEY_EPOCH,
            "device key {} has an invalid epoch",
            path.display()
        );
        let seed = URL_SAFE_NO_PAD
            .decode(stored.secret_key.trim())
            .with_context(|| format!("device key {} has a malformed secret", path.display()))?;
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| anyhow::anyhow!("device key {} has a malformed secret", path.display()))?;
        let signing = SigningKey::from_bytes(&seed);
        let stored_public = URL_SAFE_NO_PAD
            .decode(stored.public_key.trim())
            .with_context(|| format!("device key {} has a malformed public key", path.display()))?;
        ensure!(
            stored_public == signing.verifying_key().to_bytes(),
            "device key {} public and private halves disagree; refusing to use it",
            path.display()
        );
        Ok(Self {
            runner_id: stored.runner_id,
            key_epoch: stored.key_epoch,
            registered: stored.registered,
            created_at: stored.created_at,
            signing,
        })
    }

    fn to_stored(&self) -> StoredDeviceKey {
        StoredDeviceKey {
            version: DEVICE_KEY_VERSION,
            algorithm: DEVICE_KEY_ALGORITHM.to_owned(),
            runner_id: self.runner_id.clone(),
            key_epoch: self.key_epoch,
            public_key: self.public_key_b64(),
            secret_key: URL_SAFE_NO_PAD.encode(self.signing.to_bytes()),
            registered: self.registered,
            created_at: self.created_at.clone(),
        }
    }

    fn to_json(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(&self.to_stored()).context("cannot serialize the device key")
    }

    /// Create the key file (0600, owner only) and refuse to overwrite one.
    pub fn save_new(&self, path: &Path) -> Result<()> {
        ensure_supported()?;
        write_private_file(path, &self.to_json()?)
    }

    /// Atomically replace the key file: write a 0600 sibling then rename, so a
    /// crash never leaves a half-written key the Runner would refuse to load.
    pub fn save_replace(&self, path: &Path) -> Result<()> {
        ensure_supported()?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("device key path has no file name")?;
        let temporary = directory.join(format!(".{}.{}.tmp", file_name, std::process::id()));
        let _ = fs::remove_file(&temporary);
        write_private_file(&temporary, &self.to_json()?)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| format!("cannot replace {}", path.display()));
        }
        Ok(())
    }
}

/// The four signature headers of section 11.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedControlHeaders {
    pub key_epoch: u64,
    pub timestamp: String,
    pub nonce: String,
    pub signature: String,
}

impl SignedControlHeaders {
    pub fn into_pairs(self) -> [(&'static str, String); 4] {
        [
            (KEY_EPOCH_HEADER, self.key_epoch.to_string()),
            (TIMESTAMP_HEADER, self.timestamp),
            (NONCE_HEADER, self.nonce),
            (SIGNATURE_HEADER, self.signature),
        ]
    }
}

/// Where the key file lives for a given bearer credential file: beside it, not
/// inside it (section 11.8).
pub fn path_beside_credentials(credentials_file: &Path) -> PathBuf {
    credentials_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DEVICE_KEY_FILE_NAME)
}

/// Section 11.3 enroll signing string (RFC errata, ato-api#1092). The token
/// itself is never signed: only its SHA-256. The single-use enrollment token is
/// the replay guard, so enrollment carries no challenge nonce.
pub fn enrollment_signing_string(enrollment_token: &str, key_epoch: u64) -> String {
    format!(
        "{ENROLL_DOMAIN}\n{}\n{}",
        sha256_hex(enrollment_token.as_bytes()),
        key_epoch
    )
}

/// Section 11.3 register signing string.
pub fn register_signing_string(runner_id: &str, challenge_nonce: &str, key_epoch: u64) -> String {
    format!("{REGISTER_DOMAIN}\n{runner_id}\n{challenge_nonce}\n{key_epoch}")
}

/// Section 11.3 control signing string.
pub fn control_signing_string(
    method: &str,
    path_and_query: &str,
    body: &[u8],
    timestamp: &str,
    nonce: &str,
    key_epoch: u64,
) -> String {
    format!(
        "{CONTROL_DOMAIN}\n{method}\n{path_and_query}\n{}\n{timestamp}\n{nonce}\n{key_epoch}",
        sha256_hex(body)
    )
}

/// Section 11.3 rotate signing string. next_public_key is the raw 32-byte key.
pub fn rotate_signing_string(
    runner_id: &str,
    previous_epoch: u64,
    next_epoch: u64,
    next_public_key: &[u8],
    challenge_nonce: &str,
) -> String {
    format!(
        "{ROTATE_DOMAIN}\n{runner_id}\n{previous_epoch}\n{next_epoch}\n{}\n{challenge_nonce}",
        sha256_hex(next_public_key)
    )
}

/// Lowercase hex SHA-256, as every sha256_hex field in section 11.3 requires.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(<Sha256 as sha2::Digest>::digest(bytes))
}

/// Section 11.4 timestamp: RFC 3339 UTC with second precision.
pub fn now_timestamp() -> String {
    let now = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Section 11.4 nonce: base64url of 16 random bytes, no padding.
pub fn fresh_nonce() -> String {
    let mut nonce = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    URL_SAFE_NO_PAD.encode(nonce)
}

/// The paths that require section 11.4 headers once a key is active. Everything
/// else — ready reports, computation-head persistence, recovery, runtime
/// bindings, executor sessions — stays bearer-only.
pub fn is_signed_control_request(method: &str, path_and_query: &str) -> bool {
    let path = path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path);
    match method {
        "POST" => {
            (path.starts_with("/v1/runners/") && path.ends_with("/heartbeat"))
                || (path.starts_with("/v1/runner-leases/")
                    && (path.ends_with("/status") || path.ends_with("/stopped")))
        }
        "GET" => {
            (path.starts_with("/v1/runners/")
                && (path.ends_with("/leases/next") || path.ends_with("/leases/open")))
                || (path.starts_with("/v1/runner-leases/") && path.ends_with("/control"))
        }
        _ => false,
    }
}

// The 0600 contract is Unix-only, so its unit tests are too.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn temp_key_path(root: &Path) -> PathBuf {
        root.join(DEVICE_KEY_FILE_NAME)
    }

    #[test]
    fn q4_device_key_file_is_created_owner_only_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let path = temp_key_path(root.path());
        let key = RunnerDeviceKey::generate("runner_1", FIRST_KEY_EPOCH).unwrap();
        key.save_new(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "device key must be owner read/write only");

        let loaded = RunnerDeviceKey::load_if_present(&path)
            .unwrap()
            .expect("key file exists");
        assert_eq!(loaded.public_key_bytes(), key.public_key_bytes());
        assert_eq!(loaded.key_epoch(), FIRST_KEY_EPOCH);
        assert_eq!(loaded.runner_id(), "runner_1");
        assert!(!loaded.registered(), "a fresh key starts unregistered");
    }

    #[test]
    fn q4_a_looser_file_mode_is_refused_rather_than_silently_used() {
        let root = tempfile::tempdir().unwrap();
        let path = temp_key_path(root.path());
        RunnerDeviceKey::generate("runner_1", 1)
            .unwrap()
            .save_new(&path)
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = RunnerDeviceKey::load_if_present(&path).unwrap_err();
        assert!(format!("{error:#}").contains("0600"), "{error:#}");
    }

    #[test]
    fn q4_tampering_with_the_public_half_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = temp_key_path(root.path());
        let key = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        key.save_new(&path).unwrap();
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let other = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        stored["public_key"] = other.public_key_b64().into();
        fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(RunnerDeviceKey::load_if_present(&path).is_err());
    }

    #[test]
    fn q38_identity_is_stable_across_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = temp_key_path(root.path());
        let generated = RunnerDeviceKey::generate("runner_1", FIRST_KEY_EPOCH).unwrap();
        generated.save_new(&path).unwrap();

        let first = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
        let second = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
        assert_eq!(first, second, "a restart reuses the same identity");
        assert_eq!(first.public_key_b64(), generated.public_key_b64());
        // Signatures are deterministic Ed25519: same key, same message, same
        // bytes — the control plane sees one identity, not a new one per start.
        let headers = first.sign_control_at(
            "POST",
            "/v1/runners/runner_1/heartbeat",
            b"{}",
            "2026-11-03T00:00:00Z",
            "nonce",
        );
        let again = second.sign_control_at(
            "POST",
            "/v1/runners/runner_1/heartbeat",
            b"{}",
            "2026-11-03T00:00:00Z",
            "nonce",
        );
        assert_eq!(headers, again);
    }

    #[test]
    fn q4_the_private_key_never_appears_in_debug_output() {
        let key = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        let secret = URL_SAFE_NO_PAD.encode(key.signing.to_bytes());
        let debug = format!("{key:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&secret), "secret leaked into Debug");
        let error = anyhow::anyhow!("context only: {key:?}");
        assert!(!format!("{error:#}").contains(&secret));
    }

    #[test]
    fn q4_control_signature_covers_method_path_body_timestamp_nonce_and_epoch() {
        let key = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        let body = br#"{"status":"ready"}"#;
        let base = key.sign_control_at(
            "POST",
            "/v1/runner-leases/lease/status",
            body,
            "2026-11-03T00:00:00Z",
            "nonce",
        );
        let message = control_signing_string(
            "POST",
            "/v1/runner-leases/lease/status",
            body,
            "2026-11-03T00:00:00Z",
            "nonce",
            1,
        );
        assert!(key.verifies(message.as_bytes(), &base.signature));
        // A tampered body is rejected by the same public key.
        let tampered = br#"{"status":"failed"}"#;
        let tampered_message = control_signing_string(
            "POST",
            "/v1/runner-leases/lease/status",
            tampered,
            "2026-11-03T00:00:00Z",
            "nonce",
            1,
        );
        assert!(!key.verifies(tampered_message.as_bytes(), &base.signature));
        // ... and so is a different method, timestamp, nonce, or epoch.
        for message in [
            control_signing_string(
                "GET",
                "/v1/runner-leases/lease/status",
                body,
                "2026-11-03T00:00:00Z",
                "nonce",
                1,
            ),
            control_signing_string(
                "POST",
                "/v1/runner-leases/lease/status",
                body,
                "2026-11-03T00:05:00Z",
                "nonce",
                1,
            ),
            control_signing_string(
                "POST",
                "/v1/runner-leases/lease/status",
                body,
                "2026-11-03T00:00:00Z",
                "other",
                1,
            ),
            control_signing_string(
                "POST",
                "/v1/runner-leases/lease/status",
                body,
                "2026-11-03T00:00:00Z",
                "nonce",
                2,
            ),
        ] {
            assert!(!key.verifies(message.as_bytes(), &base.signature));
        }
    }

    #[test]
    fn q4_only_the_documented_control_paths_are_signed() {
        assert!(is_signed_control_request(
            "POST",
            "/v1/runners/runner/heartbeat"
        ));
        assert!(is_signed_control_request(
            "GET",
            "/v1/runners/runner/leases/next?wait_ms=20000"
        ));
        assert!(is_signed_control_request(
            "GET",
            "/v1/runners/runner/leases/open"
        ));
        assert!(is_signed_control_request(
            "POST",
            "/v1/runner-leases/lease/status"
        ));
        assert!(is_signed_control_request(
            "GET",
            "/v1/runner-leases/lease/control"
        ));
        assert!(is_signed_control_request(
            "POST",
            "/v1/runner-leases/lease/stopped"
        ));
        // Section 11.4: everything else stays bearer-only.
        assert!(!is_signed_control_request(
            "POST",
            "/v1/runner-leases/lease/ready"
        ));
        assert!(!is_signed_control_request(
            "POST",
            "/v1/runner-leases/lease/computation-head"
        ));
        assert!(!is_signed_control_request(
            "POST",
            "/v1/runner-leases/lease/recovery"
        ));
        assert!(!is_signed_control_request(
            "POST",
            "/v1/runners/runner/device-keys/challenge"
        ));
        assert!(!is_signed_control_request("POST", "/v1/runners/enroll"));
    }

    #[test]
    fn q30_rotation_signature_is_made_by_the_old_key_over_the_next_key() {
        let old = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        let next = RunnerDeviceKey::generate("runner_1", 2).unwrap();
        let signature = old.rotate_signature("runner_1", &next, "challenge");
        let message =
            rotate_signing_string("runner_1", 1, 2, &next.public_key_bytes(), "challenge");
        assert!(old.verifies(message.as_bytes(), &signature));
        // The successor cannot have signed its own rotation.
        assert!(!next.verifies(message.as_bytes(), &signature));
        // A different next epoch invalidates the proof.
        let wrong = rotate_signing_string("runner_1", 1, 3, &next.public_key_bytes(), "challenge");
        assert!(!old.verifies(wrong.as_bytes(), &signature));
    }

    #[test]
    fn q4_enroll_and_register_signatures_hash_the_token_and_bind_the_epoch() {
        let key = RunnerDeviceKey::generate("runner_1", 1).unwrap();
        let enrollment = key.enrollment_signature("enrollment-token");
        let message = enrollment_signing_string("enrollment-token", 1);
        assert!(key.verifies(message.as_bytes(), &enrollment));
        // The token itself is not the signed message, only its digest.
        assert!(!key.verifies(b"enrollment-token", &enrollment));
        // No challenge nonce participates: the token is single use.
        assert!(!key.verifies(
            enrollment_signing_string("other-token", 1).as_bytes(),
            &enrollment
        ));
        // A different epoch is a different proof.
        assert!(!key.verifies(
            enrollment_signing_string("enrollment-token", 2).as_bytes(),
            &enrollment
        ));

        let register = key.register_signature("runner_1", "challenge-nonce");
        let message = register_signing_string("runner_1", "challenge-nonce", 1);
        assert!(key.verifies(message.as_bytes(), &register));
        assert!(!key.verifies(
            register_signing_string("runner_2", "challenge-nonce", 1).as_bytes(),
            &register
        ));
    }
}
