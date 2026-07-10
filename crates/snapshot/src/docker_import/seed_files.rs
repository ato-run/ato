//! Phase 1.5 (snapshot import roadmap) — **static, recipe-owned seed files** for
//! an ephemeral tmpfs mount.
//!
//! NO external secrets, NO user input. A Store recipe declares an ephemeral
//! mount (an otherwise-empty, dies-on-resume tmpfs) and, optionally, a set of
//! files the recipe itself ships from within its own root:
//!
//! ```toml
//! [[ephemeral_mounts]]
//! path = "/config"
//! seed = "copy-up"        # or "empty"
//! size_mib = 16
//! [[ephemeral_mounts.files]]
//! path = "config.yml"          # destination RELATIVE to the mountpoint
//! source = "recipe/config.yml" # source within the recipe root ONLY
//! if_missing = true            # only write when copy-up didn't already provide it
//! ```
//!
//! The builder reads the recipe-root file **at build time**, validates it
//! fail-closed, scans the CONTENT for secret-looking literals (reusing the
//! docker_import ENV secret classifier so the two policies cannot drift), stages
//! it into the guest init, records only `{path, digest}` per file in the receipt
//! (NEVER the content), and folds the content blake3 digest into the import
//! identity — so changing a seed file's content changes the artifact identity.
//!
//! Fail-closed constraints:
//! * source: within the recipe root only — relative, no `..`, no absolute, no
//!   symlink traversal (leaf-symlink rejected + canonical containment check);
//! * dest: mountpoint-relative — relative, no `..`, no absolute, restricted to a
//!   shell-safe character set (rendered into the guest init);
//! * content: UTF-8 text only, rejected if it carries a secret-looking literal
//!   (the same invariant the baked-`ENV` gate protects).
//!
//! // TODO reconcile with Phase 1 (EphemeralMountSpec) on merge — this defines a
//! minimal self-contained ephemeral-seed-mount shape so Phase 1.5 lands
//! independently; Phase 1 owns the broader `ephemeral_mounts` concept and the two
//! should converge to a single type. This module layers the recipe-owned FILES
//! onto that concept and is orthogonal to the ato#1024 image-`VOLUME`→tmpfs path.

use std::path::Path;

use serde::Serialize;

use super::{EnvSecretClass, classify_dockerfile_env};
use crate::docker_import::rootfs::validate_ephemeral_mount_path;
use crate::rootfs_builder::validate_subdir;

/// How an ephemeral mount is initialized before the recipe seed files land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SeedMode {
    /// The tmpfs starts blank; each seed file is written after the mount.
    #[default]
    Empty,
    /// The image's existing directory content is preserved into the tmpfs first
    /// (a `cp -a` save/restore around the mount); `if_missing` files then only
    /// write when copy-up didn't already provide them.
    CopyUp,
}

/// Recipe-facing seed file spec (pre-staging): a destination relative to the
/// mountpoint and a source relative to the recipe root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFileSpec {
    /// Destination RELATIVE to the mountpoint (e.g. `config.yml`).
    pub dest: String,
    /// Source path RELATIVE to the recipe root (e.g. `recipe/config.yml`).
    pub source: String,
    /// Only write when the copy-up seed didn't already provide the file.
    pub if_missing: bool,
}

/// Recipe-facing ephemeral mount spec (pre-staging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralMountSpec {
    /// Absolute mountpoint (validated like an ato#1024 tmpfs volume path).
    pub path: String,
    pub seed: SeedMode,
    /// Optional tmpfs size cap (MiB). `None` = kernel default.
    pub size_mib: Option<u64>,
    pub files: Vec<SeedFileSpec>,
}

/// Receipt/identity-safe record of a staged seed file: destination + content
/// blake3 digest. **Never the content.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedSeedFile {
    pub dest: String,
    /// `blake3:<hex>` of the recipe-root source bytes — an identity input.
    pub digest: String,
    pub if_missing: bool,
}

/// Receipt/identity-safe record of a staged ephemeral mount (path + mode + size
/// + staged files). Folded into the import descriptor envelope, so a different
/// seed set / content / mode is a different artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedSeedMount {
    pub path: String,
    pub seed: SeedMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_mib: Option<u64>,
    pub files: Vec<StagedSeedFile>,
}

/// One seed file resolved at build time, carrying the CONTENT for guest-init
/// rendering. Kept OUT of the receipt (content is never recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSeedFile {
    /// Absolute in-guest path (`<mount>/<dest>`), shell-safe by construction.
    pub abs_dest: String,
    pub if_missing: bool,
    pub content: Vec<u8>,
}

/// One ephemeral mount resolved at build time (for guest-init rendering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSeedMount {
    pub path: String,
    pub seed: SeedMode,
    pub size_mib: Option<u64>,
    pub files: Vec<RenderedSeedFile>,
}

/// Validate a seed SOURCE path (relative to the recipe root): non-empty,
/// relative, no `..`/prefix — the same containment gate as a source subdir.
pub fn validate_seed_source(source: &str) -> Result<(), String> {
    if source.trim().is_empty() {
        return Err("seed file source is empty".into());
    }
    validate_subdir(source).map_err(|e| format!("seed file source {source:?}: {e}"))
}

/// Validate a seed DEST path (relative to the mountpoint): non-empty, relative,
/// no `..`/prefix, and restricted to `[A-Za-z0-9/_.-]` (it is rendered into the
/// guest init — fail-closed on shell metacharacters rather than escaping).
pub fn validate_seed_dest(dest: &str) -> Result<(), String> {
    if dest.trim().is_empty() {
        return Err("seed file dest is empty".into());
    }
    validate_subdir(dest).map_err(|e| format!("seed file dest {dest:?}: {e}"))?;
    if !dest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-')) {
        return Err(format!(
            "seed file dest {dest:?} contains characters outside [A-Za-z0-9/_.-] — refusing to \
             render it into the guest init (fail-closed)"
        ));
    }
    Ok(())
}

/// Join a validated absolute mountpoint and a validated mount-relative dest into
/// the absolute in-guest path. Both halves are pre-restricted to a shell-safe
/// character set, so the result never needs escaping.
fn join_mount_dest(mount: &str, dest: &str) -> String {
    format!("{}/{}", mount.trim_end_matches('/'), dest.trim_start_matches('/'))
}

/// Reject secret-looking CONTENT in a recipe seed file. A static, recipe-owned
/// config file that carries a literal credential would be sealed into the
/// artifact exactly like a baked `ENV` secret — the invariant the docker_import
/// ENV gate protects. Reuses the SAME classifier ([`classify_dockerfile_env`])
/// so the marker/prefix tables cannot drift:
/// * a `key: value` / `key=value` line whose key is secret-looking with a
///   non-placeholder value rejects the file;
/// * any bare token shaped like a provider credential (`sk-…`, `ghp_…`, …)
///   rejects the file.
///
/// Comment lines (`#`-prefixed) are skipped. Content must be UTF-8 text: a
/// binary blob is unscannable, and a static config seed is text by nature.
fn scan_seed_content(source: &str, content: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(content).map_err(|_| {
        format!("seed file {source:?} is not valid UTF-8 (only text config seeds are allowed)")
    })?;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lineno = i + 1;
        // `key: value` (YAML) or `key=value` (env/ini): a secret-looking key with
        // a literal value is a sealed secret.
        if let Some((k, v)) = split_kv(line)
            && classify_dockerfile_env(k.trim(), strip_quotes(v.trim())) != EnvSecretClass::Plain
        {
            return Err(format!(
                "seed file {source:?} line {lineno} sets a secret-looking key to a \
                 credential-shaped value — refusing to seal a secret (declare an Ato \
                 [secrets.*] binding instead)"
            ));
        }
        // A provider-credential-shaped literal anywhere on the line (even without
        // a key half): classified with an empty key so only the value shape gates.
        for tok in line.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')) {
            let tok = tok.trim_matches(|c| matches!(c, ':' | '='));
            if !tok.is_empty() && classify_dockerfile_env("", tok) != EnvSecretClass::Plain {
                return Err(format!(
                    "seed file {source:?} line {lineno} contains a provider-credential-shaped \
                     literal — refusing to seal a secret (declare an Ato [secrets.*] binding instead)"
                ));
            }
        }
    }
    Ok(())
}

/// Split `key: value` / `key = value` / `key=value` at the first `:` or `=`.
fn split_kv(line: &str) -> Option<(&str, &str)> {
    let idx = line.char_indices().find(|(_, c)| matches!(c, ':' | '=')).map(|(i, _)| i)?;
    let (k, rest) = line.split_at(idx);
    let v = &rest[1..];
    if k.trim().is_empty() {
        return None;
    }
    Some((k, v))
}

/// Strip a single pair of wrapping quotes from a config value so the classifier
/// sees the bare literal (`"sk-…"` → `sk-…`).
fn strip_quotes(v: &str) -> &str {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Stage ONE seed file from the recipe root: validate the source + dest paths,
/// resolve the source WITHOUT following a symlink out of the root (leaf-symlink
/// rejected + canonical containment), read the bytes, scan for secret-looking
/// content, and compute the content digest. Returns the receipt-safe record and
/// the content-carrying render record.
pub fn stage_seed_file(
    recipe_root: &Path,
    mount: &str,
    spec: &SeedFileSpec,
) -> Result<(StagedSeedFile, RenderedSeedFile), String> {
    validate_seed_source(&spec.source)?;
    validate_seed_dest(&spec.dest)?;

    let root_canon = recipe_root
        .canonicalize()
        .map_err(|e| format!("recipe root {}: {e}", recipe_root.display()))?;
    let src = root_canon.join(&spec.source);
    // The leaf must be a regular file, not a symlink (closes symlink-to-secret
    // traversal even when the target would land back inside the root).
    let meta = std::fs::symlink_metadata(&src)
        .map_err(|e| format!("seed file source {:?}: {e}", spec.source))?;
    if meta.file_type().is_symlink() {
        return Err(format!("seed file source {:?} is a symlink — refusing (fail-closed)", spec.source));
    }
    if !meta.file_type().is_file() {
        return Err(format!("seed file source {:?} is not a regular file", spec.source));
    }
    // Canonical containment (resolves any intermediate directory symlink) — the
    // resolved source must still live inside the recipe root.
    let src_canon = src
        .canonicalize()
        .map_err(|e| format!("seed file source {:?}: {e}", spec.source))?;
    if !src_canon.starts_with(&root_canon) {
        return Err(format!("seed file source {:?} escapes the recipe root (fail-closed)", spec.source));
    }

    let content = std::fs::read(&src_canon).map_err(|e| format!("read seed file {:?}: {e}", spec.source))?;
    scan_seed_content(&spec.source, &content)?;
    let digest = format!("blake3:{}", blake3::hash(&content).to_hex());
    let abs_dest = join_mount_dest(mount, &spec.dest);
    Ok((
        StagedSeedFile { dest: spec.dest.clone(), digest, if_missing: spec.if_missing },
        RenderedSeedFile { abs_dest, if_missing: spec.if_missing, content },
    ))
}

/// Stage a whole ephemeral mount + its seed files. Validates the mountpoint
/// (same fail-closed rules as an ato#1024 tmpfs VOLUME path), then stages every
/// file. Rejects a duplicate destination within one mount (a silent
/// last-write-wins would be surprising). Returns the receipt-safe mount record
/// and the render record.
pub fn stage_seed_mount(
    recipe_root: &Path,
    spec: &EphemeralMountSpec,
) -> Result<(StagedSeedMount, RenderedSeedMount), String> {
    validate_ephemeral_mount_path(&spec.path)?;
    let mut staged = Vec::with_capacity(spec.files.len());
    let mut rendered = Vec::with_capacity(spec.files.len());
    let mut seen: Vec<&str> = Vec::new();
    for f in &spec.files {
        if seen.contains(&f.dest.as_str()) {
            return Err(format!(
                "ephemeral mount {:?} declares seed dest {:?} twice (fail-closed)",
                spec.path, f.dest
            ));
        }
        seen.push(&f.dest);
        let (s, r) = stage_seed_file(recipe_root, &spec.path, f)?;
        staged.push(s);
        rendered.push(r);
    }
    Ok((
        StagedSeedMount { path: spec.path.clone(), seed: spec.seed, size_mib: spec.size_mib, files: staged },
        RenderedSeedMount { path: spec.path.clone(), seed: spec.seed, size_mib: spec.size_mib, files: rendered },
    ))
}

/// Base64-encode with the standard alphabet (no external dep) so ARBITRARY file
/// bytes (quotes, newlines, non-ASCII) round-trip through the quoted guest-init
/// heredoc and are decoded in the guest with `base64 -d`.
fn base64_encode(data: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHA[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHA[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Render the guest-init lines for one ephemeral seed mount: mount the tmpfs,
/// (optionally) preserve the image's existing directory content via copy-up,
/// then write each recipe seed file. Content is embedded base64 and decoded in
/// the guest, so it needs no shell escaping; paths are pre-validated shell-safe.
///
/// Emitted into the init AFTER the standard tmpfs mounts and BEFORE `cd` (the
/// same region as the ato#1024 VOLUME mounts). Requires `base64` in the guest
/// image (present in coreutils/busybox); a file that fails to decode simply does
/// not appear and the app's readiness probe fails honestly.
pub fn render_seed_mount_init(m: &RenderedSeedMount) -> String {
    let mut out = String::new();
    let mount = m.path.trim_end_matches('/');
    let size_opt = m.size_mib.map(|s| format!("-o size={s}m ")).unwrap_or_default();
    match m.seed {
        SeedMode::Empty => {
            out.push_str(&format!(
                "mkdir -p {mount} 2>/dev/null; mount -t tmpfs {size_opt}tmpfs {mount} 2>/dev/null\n"
            ));
        }
        SeedMode::CopyUp => {
            // Preserve the baked directory content into the fresh tmpfs.
            let stash = format!("/tmp/.ato-seed-copyup{}", mount.replace('/', "_"));
            out.push_str(&format!("mkdir -p {mount} 2>/dev/null\n"));
            out.push_str(&format!("rm -rf {stash} 2>/dev/null; cp -a {mount} {stash} 2>/dev/null || true\n"));
            out.push_str(&format!("mount -t tmpfs {size_opt}tmpfs {mount} 2>/dev/null\n"));
            out.push_str(&format!("cp -a {stash}/. {mount}/ 2>/dev/null || true; rm -rf {stash} 2>/dev/null || true\n"));
        }
    }
    for f in &m.files {
        let b64 = base64_encode(&f.content);
        // The dest's parent must exist inside the tmpfs before the write.
        out.push_str(&format!(
            "mkdir -p \"$(dirname '{dest}')\" 2>/dev/null\n",
            dest = f.abs_dest
        ));
        let write = format!("printf %s '{b64}' | base64 -d > '{dest}' 2>/dev/null\n", dest = f.abs_dest);
        if f.if_missing {
            out.push_str(&format!("[ -e '{dest}' ] || {{ {write}}}", dest = f.abs_dest, write = write));
        } else {
            out.push_str(&write);
        }
    }
    out
}

/// Render the init lines for every ephemeral seed mount, in order.
pub fn render_seed_mounts_init(mounts: &[RenderedSeedMount]) -> String {
    mounts.iter().map(render_seed_mount_init).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// A throwaway recipe root with a couple of files.
    fn recipe_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("recipe")).unwrap();
        fs::write(dir.path().join("recipe/config.yml"), "port: 3000\nrequire_auth: false\n").unwrap();
        dir
    }

    fn spec(dest: &str, source: &str, if_missing: bool) -> SeedFileSpec {
        SeedFileSpec { dest: dest.into(), source: source.into(), if_missing }
    }

    // --- source containment ---------------------------------------------------

    #[test]
    fn source_outside_recipe_root_is_rejected() {
        let dir = recipe_root();
        // A lexical absolute / parent escape is rejected by the path validator.
        for bad in ["/etc/passwd", "../outside.yml", "recipe/../../x"] {
            let err = stage_seed_file(dir.path(), "/config", &spec("config.yml", bad, false)).unwrap_err();
            assert!(err.contains("seed file source"), "{bad}: {err}");
        }
    }

    #[test]
    fn parent_dir_in_dest_is_rejected() {
        let dir = recipe_root();
        let err = stage_seed_file(dir.path(), "/config", &spec("../evil.yml", "recipe/config.yml", false)).unwrap_err();
        assert!(err.contains("seed file dest") && err.contains(".."), "{err}");
    }

    #[test]
    fn absolute_dest_is_rejected() {
        let dir = recipe_root();
        let err = stage_seed_file(dir.path(), "/config", &spec("/abs.yml", "recipe/config.yml", false)).unwrap_err();
        assert!(err.contains("seed file dest"), "{err}");
    }

    #[test]
    fn leaf_symlink_source_is_rejected() {
        let dir = recipe_root();
        // A symlink inside the recipe root that points at an outside secret.
        let outside = dir.path().join("outside_secret");
        fs::write(&outside, "port: 1\n").unwrap();
        symlink(&outside, dir.path().join("recipe/link.yml")).unwrap();
        let err = stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/link.yml", false)).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    #[test]
    fn intermediate_symlink_dir_escaping_root_is_rejected() {
        let dir = recipe_root();
        let escape = tempfile::tempdir().unwrap();
        fs::write(escape.path().join("secret.yml"), "port: 1\n").unwrap();
        // recipe/esc -> <outside dir>; recipe/esc/secret.yml canonicalizes outside.
        symlink(escape.path(), dir.path().join("recipe/esc")).unwrap();
        let err = stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/esc/secret.yml", false)).unwrap_err();
        // Either the leaf-file check (its parent is a symlink dir) or the canonical
        // containment check fires — both are fail-closed rejections.
        assert!(err.contains("seed file source"), "{err}");
    }

    // --- content secret scan --------------------------------------------------

    #[test]
    fn secret_looking_content_is_rejected() {
        let dir = recipe_root();
        for (name, body) in [
            ("k1.yml", "api_key: sk-abcdefghijklmnopqrstuvwxyz012345\n"),
            ("k2.yml", "AUTH_TOKEN: hunter2hunter2hunter2\n"),
            ("k3.yml", "note: ghp_0123456789abcdefghij0123456789abcd\n"),
        ] {
            fs::write(dir.path().join("recipe").join(name), body).unwrap();
            let err = stage_seed_file(dir.path(), "/config", &spec("config.yml", &format!("recipe/{name}"), false))
                .unwrap_err();
            assert!(err.contains("secret"), "{name}: {err}");
            // The secret value must never be echoed back.
            assert!(!err.contains("sk-abcdefghijklmnopqrstuvwxyz"), "{name}: {err}");
        }
    }

    #[test]
    fn comment_lines_and_plain_config_pass() {
        let dir = recipe_root();
        fs::write(
            dir.path().join("recipe/ok.yml"),
            "# api_key: sk-notarealsecretjustacomment\nport: 3000\nrequire_auth: false\n",
        )
        .unwrap();
        assert!(stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/ok.yml", false)).is_ok());
    }

    // --- digest + identity ----------------------------------------------------

    #[test]
    fn digest_records_content_and_changes_with_content() {
        let dir = recipe_root();
        let (staged, _) = stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/config.yml", true)).unwrap();
        assert_eq!(staged.dest, "config.yml");
        assert!(staged.digest.starts_with("blake3:") && staged.digest.len() == 7 + 64, "{}", staged.digest);
        assert!(staged.if_missing);
        let before = staged.digest.clone();
        // Changing the content changes the digest (⇒ changes the identity envelope).
        fs::write(dir.path().join("recipe/config.yml"), "port: 4000\n").unwrap();
        let (staged2, _) = stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/config.yml", true)).unwrap();
        assert_ne!(staged2.digest, before);
    }

    #[test]
    fn staged_record_never_serializes_content() {
        let dir = recipe_root();
        let (staged, _) = stage_seed_file(dir.path(), "/config", &spec("config.yml", "recipe/config.yml", true)).unwrap();
        let json = serde_json::to_string(&staged).unwrap();
        assert!(json.contains("digest") && json.contains("config.yml"));
        assert!(!json.contains("port: 3000"), "content must never appear in the receipt record: {json}");
    }

    // --- rendering + if_missing semantics -------------------------------------

    #[test]
    fn empty_seed_renders_mount_and_unguarded_write() {
        let dir = recipe_root();
        let m = EphemeralMountSpec {
            path: "/config".into(),
            seed: SeedMode::Empty,
            size_mib: Some(16),
            files: vec![spec("config.yml", "recipe/config.yml", false)],
        };
        let (_staged, rendered) = stage_seed_mount(dir.path(), &m).unwrap();
        let init = render_seed_mount_init(&rendered);
        assert!(init.contains("mount -t tmpfs -o size=16m tmpfs /config"), "{init}");
        assert!(init.contains("base64 -d > '/config/config.yml'"), "{init}");
        // if_missing=false ⇒ no `[ -e … ] ||` guard.
        assert!(!init.contains("[ -e '/config/config.yml' ]"), "{init}");
    }

    #[test]
    fn if_missing_true_guards_the_write() {
        let dir = recipe_root();
        let m = EphemeralMountSpec {
            path: "/config".into(),
            seed: SeedMode::CopyUp,
            size_mib: None,
            files: vec![spec("config.yml", "recipe/config.yml", true)],
        };
        let (_staged, rendered) = stage_seed_mount(dir.path(), &m).unwrap();
        let init = render_seed_mount_init(&rendered);
        // copy-up preserves the baked dir, then only writes when absent.
        assert!(init.contains("cp -a /config /tmp/.ato-seed-copyup_config"), "{init}");
        assert!(init.contains("[ -e '/config/config.yml' ] ||"), "{init}");
        // No size option when size_mib is None.
        assert!(init.contains("mount -t tmpfs tmpfs /config"), "{init}");
    }

    #[test]
    fn base64_roundtrips_arbitrary_bytes() {
        // The encoder must match the guest's `base64 -d`.
        for data in [&b""[..], b"a", b"ab", b"abc", b"port: 3000\n\"quotes'\t\x00\xff"] {
            let b64 = base64_encode(data);
            let back = decode_b64(&b64);
            assert_eq!(back, data, "roundtrip failed for {data:?} -> {b64}");
        }
    }

    #[test]
    fn duplicate_dest_in_one_mount_is_rejected() {
        let dir = recipe_root();
        let m = EphemeralMountSpec {
            path: "/config".into(),
            seed: SeedMode::Empty,
            size_mib: None,
            files: vec![
                spec("config.yml", "recipe/config.yml", false),
                spec("config.yml", "recipe/config.yml", true),
            ],
        };
        let err = stage_seed_mount(dir.path(), &m).unwrap_err();
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn bad_mountpoint_is_rejected() {
        let dir = recipe_root();
        for bad in ["relative", "/", "/etc/app", "/config bad"] {
            let m = EphemeralMountSpec {
                path: bad.into(),
                seed: SeedMode::Empty,
                size_mib: None,
                files: vec![],
            };
            assert!(stage_seed_mount(dir.path(), &m).is_err(), "{bad}");
        }
    }

    /// Reference base64 decoder for the roundtrip test (mirrors `base64 -d`).
    fn decode_b64(s: &str) -> Vec<u8> {
        fn val(c: u8) -> Option<u32> {
            match c {
                b'A'..=b'Z' => Some((c - b'A') as u32),
                b'a'..=b'z' => Some((c - b'a' + 26) as u32),
                b'0'..=b'9' => Some((c - b'0' + 52) as u32),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let mut n = 0u32;
            let mut bits = 0;
            for &c in chunk {
                n = (n << 6) | val(c).unwrap();
                bits += 6;
            }
            let mut shift = bits as i32 - 8;
            while shift >= 0 {
                out.push(((n >> shift as u32) & 0xff) as u8);
                shift -= 8;
            }
        }
        out
    }
}
