//! Build-time **staging** of static, recipe-owned seed files for ephemeral
//! tmpfs mounts — the filesystem half of the unified `ephemeral_mounts`
//! contract (`rootfs::EphemeralMountSpec` / `rootfs::EphemeralSeedFile`).
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
//! Structural validation (mount paths, destinations, duplicates, nesting) is
//! `rootfs::validate_ephemeral_mounts` — the single gate. THIS module does the
//! work that needs the recipe checkout at build time: resolve each source
//! WITHOUT following a symlink out of the root, read the bytes, scan the
//! CONTENT for secret-looking literals (reusing the docker_import ENV secret
//! classifier so the two policies cannot drift), fill the spec's blake3
//! `source_digest` (an identity input — the receipt records only
//! `{path, source_path, source_digest, if_missing}`, NEVER the content), and
//! hand the content to the init renderer (`rootfs::render_ephemeral_mount`
//! renders mount + copy-up + file writes from the one normalized plan).

use std::path::Path;

use super::rootfs::{EphemeralMountSpec, EphemeralSeedFile};
use super::{EnvSecretClass, classify_dockerfile_env};
use crate::rootfs_builder::validate_subdir;

/// One seed file resolved at build time, carrying the CONTENT for guest-init
/// rendering. Kept OUT of the receipt (content is never recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSeedFile {
    /// Absolute in-guest path (`<mount>/<dest>`), shell-safe by construction.
    pub abs_dest: String,
    pub if_missing: bool,
    pub content: Vec<u8>,
}

/// The staged file CONTENTS for one ephemeral mount, keyed by mountpoint —
/// the side table `pack_imported_rootfs` aligns (fail-closed) against the
/// plan's declared `files` before rendering. Never serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMountSeeds {
    pub path: String,
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
    if !dest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    {
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
    format!(
        "{}/{}",
        mount.trim_end_matches('/'),
        dest.trim_start_matches('/')
    )
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
        for tok in line.split(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
        }) {
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
    let idx = line
        .char_indices()
        .find(|(_, c)| matches!(c, ':' | '='))
        .map(|(i, _)| i)?;
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
/// content, and fill the content digest. Returns the digest-filled spec (the
/// receipt/identity record) and the content-carrying render record.
pub fn stage_seed_file(
    recipe_root: &Path,
    mount: &str,
    spec: &EphemeralSeedFile,
) -> Result<(EphemeralSeedFile, RenderedSeedFile), String> {
    validate_seed_source(&spec.source_path)?;
    validate_seed_dest(&spec.path)?;

    let root_canon = recipe_root
        .canonicalize()
        .map_err(|e| format!("recipe root {}: {e}", recipe_root.display()))?;
    let src = root_canon.join(&spec.source_path);
    // The leaf must be a regular file, not a symlink (closes symlink-to-secret
    // traversal even when the target would land back inside the root).
    let meta = std::fs::symlink_metadata(&src)
        .map_err(|e| format!("seed file source {:?}: {e}", spec.source_path))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "seed file source {:?} is a symlink — refusing (fail-closed)",
            spec.source_path
        ));
    }
    if !meta.file_type().is_file() {
        return Err(format!(
            "seed file source {:?} is not a regular file",
            spec.source_path
        ));
    }
    // Canonical containment (resolves any intermediate directory symlink) — the
    // resolved source must still live inside the recipe root.
    let src_canon = src
        .canonicalize()
        .map_err(|e| format!("seed file source {:?}: {e}", spec.source_path))?;
    if !src_canon.starts_with(&root_canon) {
        return Err(format!(
            "seed file source {:?} escapes the recipe root (fail-closed)",
            spec.source_path
        ));
    }

    let content = std::fs::read(&src_canon)
        .map_err(|e| format!("read seed file {:?}: {e}", spec.source_path))?;
    scan_seed_content(&spec.source_path, &content)?;
    let digest = format!("blake3:{}", blake3::hash(&content).to_hex());
    let abs_dest = join_mount_dest(mount, &spec.path);
    Ok((
        EphemeralSeedFile {
            path: spec.path.clone(),
            source_path: spec.source_path.clone(),
            source_digest: digest,
            if_missing: spec.if_missing,
        },
        RenderedSeedFile {
            abs_dest,
            if_missing: spec.if_missing,
            content,
        },
    ))
}

/// Stage every seed file of ONE normalized mount: returns the mount with each
/// file's `source_digest` filled (the receipt/identity record) plus this
/// mount's content side table. A mount with no files passes through unchanged
/// (image-VOLUME mounts / plain explicit mounts never touch the filesystem).
pub fn stage_mount_seeds(
    recipe_root: &Path,
    mount: &EphemeralMountSpec,
) -> Result<(EphemeralMountSpec, RenderedMountSeeds), String> {
    let mut staged = Vec::with_capacity(mount.files.len());
    let mut rendered = Vec::with_capacity(mount.files.len());
    for f in &mount.files {
        let (s, r) = stage_seed_file(recipe_root, &mount.path, f)?;
        staged.push(s);
        rendered.push(r);
    }
    let mut out = mount.clone();
    out.files = staged;
    Ok((
        out,
        RenderedMountSeeds {
            path: mount.path.clone(),
            files: rendered,
        },
    ))
}

/// Stage the WHOLE normalized mount list (the plan's `ephemeral_mounts`, already
/// sorted + structurally validated): fills every `source_digest` and returns the
/// aligned content table for `pack_imported_rootfs`.
pub fn stage_all_mounts(
    recipe_root: &Path,
    mounts: &[EphemeralMountSpec],
) -> Result<(Vec<EphemeralMountSpec>, Vec<RenderedMountSeeds>), String> {
    let mut staged = Vec::with_capacity(mounts.len());
    let mut rendered = Vec::with_capacity(mounts.len());
    for m in mounts {
        let (s, r) = stage_mount_seeds(recipe_root, m)?;
        staged.push(s);
        rendered.push(r);
    }
    Ok((staged, rendered))
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
        out.push(if chunk.len() > 1 {
            ALPHA[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Render the guest-init lines that write ONE staged seed file (called by
/// `rootfs::render_ephemeral_mount` AFTER the mount + copy-up lines, so the
/// whole mount renders from one place). Content is embedded base64 and decoded
/// in-guest (`base64` is present in coreutils/busybox); the dest path is
/// pre-validated shell-safe. A failed directory create or write FAILS guest
/// boot — a seed file is part of the artifact's identity, so an artifact
/// missing it must never seal (fail-closed, never `2>/dev/null`).
pub(crate) fn render_seed_file_write(f: &RenderedSeedFile) -> String {
    let dest = &f.abs_dest;
    let b64 = base64_encode(&f.content);
    let write = format!(
        "mkdir -p \"$(dirname '{dest}')\" || {{ echo \"seed file dir create failed: {dest}\" >&2; exit 1; }}\n\
         printf %s '{b64}' | base64 -d > '{dest}' || {{ echo \"seed file write failed: {dest}\" >&2; exit 1; }}\n"
    );
    if f.if_missing {
        format!("if [ ! -e '{dest}' ]; then\n{write}fi\n")
    } else {
        write
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker_import::rootfs::{EphemeralMountSeed, EphemeralMountSource};
    use std::fs;
    use std::os::unix::fs::symlink;

    /// A throwaway recipe root with a couple of files.
    fn recipe_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("recipe")).unwrap();
        fs::write(
            dir.path().join("recipe/config.yml"),
            "port: 3000\nrequire_auth: false\n",
        )
        .unwrap();
        dir
    }

    fn spec(dest: &str, source: &str, if_missing: bool) -> EphemeralSeedFile {
        EphemeralSeedFile {
            path: dest.into(),
            source_path: source.into(),
            source_digest: String::new(),
            if_missing,
        }
    }

    fn mount(
        path: &str,
        seed: EphemeralMountSeed,
        size_mib: Option<u32>,
        files: Vec<EphemeralSeedFile>,
    ) -> EphemeralMountSpec {
        EphemeralMountSpec {
            path: path.into(),
            seed,
            size_mib,
            source: EphemeralMountSource::Explicit,
            files,
        }
    }

    // --- source containment ---------------------------------------------------

    #[test]
    fn source_outside_recipe_root_is_rejected() {
        let dir = recipe_root();
        // A lexical absolute / parent escape is rejected by the path validator.
        for bad in ["/etc/passwd", "../outside.yml", "recipe/../../x"] {
            let err = stage_seed_file(dir.path(), "/config", &spec("config.yml", bad, false))
                .unwrap_err();
            assert!(err.contains("seed file source"), "{bad}: {err}");
        }
    }

    #[test]
    fn parent_dir_in_dest_is_rejected() {
        let dir = recipe_root();
        let err = stage_seed_file(
            dir.path(),
            "/config",
            &spec("../evil.yml", "recipe/config.yml", false),
        )
        .unwrap_err();
        assert!(
            err.contains("seed file dest") && err.contains(".."),
            "{err}"
        );
    }

    #[test]
    fn absolute_dest_is_rejected() {
        let dir = recipe_root();
        let err = stage_seed_file(
            dir.path(),
            "/config",
            &spec("/abs.yml", "recipe/config.yml", false),
        )
        .unwrap_err();
        assert!(err.contains("seed file dest"), "{err}");
    }

    #[test]
    fn leaf_symlink_source_is_rejected() {
        let dir = recipe_root();
        // A symlink inside the recipe root that points at an outside secret.
        let outside = dir.path().join("outside_secret");
        fs::write(&outside, "port: 1\n").unwrap();
        symlink(&outside, dir.path().join("recipe/link.yml")).unwrap();
        let err = stage_seed_file(
            dir.path(),
            "/config",
            &spec("config.yml", "recipe/link.yml", false),
        )
        .unwrap_err();
        assert!(err.contains("symlink"), "{err}");
    }

    #[test]
    fn intermediate_symlink_dir_escaping_root_is_rejected() {
        let dir = recipe_root();
        let escape = tempfile::tempdir().unwrap();
        fs::write(escape.path().join("secret.yml"), "port: 1\n").unwrap();
        // recipe/esc -> <outside dir>; recipe/esc/secret.yml canonicalizes outside.
        symlink(escape.path(), dir.path().join("recipe/esc")).unwrap();
        let err = stage_seed_file(
            dir.path(),
            "/config",
            &spec("config.yml", "recipe/esc/secret.yml", false),
        )
        .unwrap_err();
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
            let err = stage_seed_file(
                dir.path(),
                "/config",
                &spec("config.yml", &format!("recipe/{name}"), false),
            )
            .unwrap_err();
            assert!(err.contains("secret"), "{name}: {err}");
            // The secret value must never be echoed back.
            assert!(
                !err.contains("sk-abcdefghijklmnopqrstuvwxyz"),
                "{name}: {err}"
            );
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
        assert!(
            stage_seed_file(
                dir.path(),
                "/config",
                &spec("config.yml", "recipe/ok.yml", false)
            )
            .is_ok()
        );
    }

    // --- digest + identity ----------------------------------------------------

    #[test]
    fn digest_records_content_and_changes_with_content() {
        let dir = recipe_root();
        let (staged, _) = stage_seed_file(
            dir.path(),
            "/config",
            &spec("config.yml", "recipe/config.yml", true),
        )
        .unwrap();
        assert_eq!(staged.path, "config.yml");
        assert_eq!(staged.source_path, "recipe/config.yml");
        assert!(
            staged.source_digest.starts_with("blake3:") && staged.source_digest.len() == 7 + 64,
            "{}",
            staged.source_digest
        );
        assert!(staged.if_missing);
        let before = staged.source_digest.clone();
        // Changing the content changes the digest (⇒ changes the identity envelope).
        fs::write(dir.path().join("recipe/config.yml"), "port: 4000\n").unwrap();
        let (staged2, _) = stage_seed_file(
            dir.path(),
            "/config",
            &spec("config.yml", "recipe/config.yml", true),
        )
        .unwrap();
        assert_ne!(staged2.source_digest, before);
    }

    #[test]
    fn staged_record_never_serializes_content() {
        let dir = recipe_root();
        let (staged, _) = stage_seed_file(
            dir.path(),
            "/config",
            &spec("config.yml", "recipe/config.yml", true),
        )
        .unwrap();
        let json = serde_json::to_string(&staged).unwrap();
        assert!(json.contains("source_digest") && json.contains("config.yml"));
        assert!(
            !json.contains("port: 3000"),
            "content must never appear in the receipt record: {json}"
        );
    }

    // --- staging a whole mount list --------------------------------------------

    #[test]
    fn stage_all_mounts_fills_digests_and_aligns_content() {
        let dir = recipe_root();
        let mounts = vec![
            mount(
                "/config",
                EphemeralMountSeed::CopyUp,
                Some(16),
                vec![spec("config.yml", "recipe/config.yml", true)],
            ),
            mount("/downloads", EphemeralMountSeed::Empty, Some(512), vec![]),
        ];
        let (staged, rendered) = stage_all_mounts(dir.path(), &mounts).unwrap();
        assert_eq!(staged.len(), 2);
        assert_eq!(rendered.len(), 2);
        assert!(staged[0].files[0].source_digest.starts_with("blake3:"));
        assert_eq!(rendered[0].path, "/config");
        assert_eq!(rendered[0].files[0].abs_dest, "/config/config.yml");
        assert_eq!(
            rendered[0].files[0].content,
            b"port: 3000\nrequire_auth: false\n"
        );
        // The file-less mount passes through untouched with no content entries.
        assert_eq!(staged[1], mounts[1]);
        assert!(rendered[1].files.is_empty());
    }

    // --- rendering + if_missing semantics -------------------------------------

    #[test]
    fn unguarded_write_renders_fail_closed() {
        let f = RenderedSeedFile {
            abs_dest: "/config/config.yml".into(),
            if_missing: false,
            content: b"port: 3000\n".to_vec(),
        };
        let init = render_seed_file_write(&f);
        assert!(init.contains("base64 -d > '/config/config.yml'"), "{init}");
        assert!(
            init.contains("seed file write failed") && init.contains("exit 1"),
            "must fail boot on a failed write: {init}"
        );
        // if_missing=false ⇒ no existence guard.
        assert!(!init.contains("[ ! -e '/config/config.yml' ]"), "{init}");
    }

    #[test]
    fn if_missing_true_guards_the_write() {
        let f = RenderedSeedFile {
            abs_dest: "/config/config.yml".into(),
            if_missing: true,
            content: b"port: 3000\n".to_vec(),
        };
        let init = render_seed_file_write(&f);
        assert!(
            init.contains("if [ ! -e '/config/config.yml' ]; then"),
            "{init}"
        );
        assert!(init.trim_end().ends_with("fi"), "{init}");
    }

    #[test]
    fn base64_roundtrips_arbitrary_bytes() {
        // The encoder must match the guest's `base64 -d`.
        for data in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"port: 3000\n\"quotes'\t\x00\xff",
        ] {
            let b64 = base64_encode(data);
            let back = decode_b64(&b64);
            assert_eq!(back, data, "roundtrip failed for {data:?} -> {b64}");
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
            let mut shift = bits - 8;
            while shift >= 0 {
                out.push(((n >> shift as u32) & 0xff) as u8);
                shift -= 8;
            }
        }
        out
    }
}
