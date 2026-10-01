//! Redeemed variables use the existing non-serializable secret spawn boundary.
use crate::launch::resolved::ResolvedSecret;
use anyhow::{Result, ensure};
use ato_formation::{requirements::ExecutionPhase, variables::VariableRequirement};

#[derive(Debug)]
pub struct ResolvedVariable {
    pub phase: ExecutionPhase,
    pub grant_ref: String,
    pub value: ResolvedSecret,
}
impl ResolvedVariable {
    pub fn new(
        requirement: &VariableRequirement,
        grant_ref: String,
        value: String,
    ) -> Result<Self> {
        ensure!(
            grant_ref.starts_with("formation-variable:")
                && !value.is_empty()
                && value.len() <= 16384
                && !value.contains('\0'),
            "variable_resolution_invalid"
        );
        Ok(Self {
            phase: requirement.phase,
            grant_ref,
            value: ResolvedSecret::new(&requirement.name, value),
        })
    }
}
pub fn redact(text: &str, variables: &[ResolvedVariable]) -> String {
    let mut text = text.to_owned();
    // Longest first prevents a shorter overlapping value hiding a longer one.
    let mut values = variables
        .iter()
        .map(|v| v.value.expose_for_spawn())
        .collect::<Vec<_>>();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    for value in values {
        text = text.replace(value, "<redacted>");
    }
    text
}

/// Both secret values and explicitly non-embeddable public configuration stay
/// out of reusable artifacts. Missing declarations fail closed as protected.
pub fn artifact_guard_values<'a>(
    requirements: &[VariableRequirement],
    variables: &'a [ResolvedVariable],
) -> Vec<&'a [u8]> {
    variables
        .iter()
        .filter(|v| {
            requirements
                .iter()
                .find(|r| r.name == v.value.name())
                .is_none_or(|r| r.secret || !r.artifact_embedding)
        })
        .map(|v| v.value.expose_for_spawn().as_bytes())
        .collect()
}

/// Scan a process artifact before it can become reusable materialization.
/// Values stay in private spawn context; bounds fail closed without printing bytes.
pub fn scan_artifact(root: &std::path::Path, secrets: &[&[u8]]) -> Result<()> {
    scan_artifact_controlled(root, secrets, None)
}
pub fn scan_artifact_controlled(
    root: &std::path::Path,
    secrets: &[&[u8]],
    control: Option<&crate::control::ExecutionControl>,
) -> Result<()> {
    use std::io::Read;
    if let Some(c) = control {
        c.remaining(crate::control::AttemptPhase::Build)?;
    }
    if secrets.is_empty() {
        return Ok(());
    }
    let overlap = secrets
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(1)
        .saturating_sub(1);
    let mut pending = vec![root.to_owned()];
    let mut entries = 0u64;
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        if let Some(c) = control {
            c.remaining(crate::control::AttemptPhase::Build)?;
        }
        let relative = path.strip_prefix(root).unwrap_or(std::path::Path::new(""));
        if !ato_materializer_static_web::blob_is_clean(
            relative.as_os_str().as_encoded_bytes(),
            secrets,
        ) {
            return Err(ato_materializer_static_web::ArtifactEmbeddingRefused.into());
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        entries += 1;
        ensure!(entries <= 100_000, "variable_artifact_scan_bound");
        if metadata.is_symlink() {
            // Reuse the source/retained archive's containment rule. Do not
            // follow aliases: the ordinary tree walk scans every regular
            // target, while checking link bytes also protects their identity.
            let target = std::fs::read_link(&path)?;
            let target_bytes = target.as_os_str().as_encoded_bytes();
            if !ato_materializer_static_web::blob_is_clean(target_bytes, secrets) {
                return Err(ato_materializer_static_web::ArtifactEmbeddingRefused.into());
            }
            ensure!(
                ato_formation::containment::validate_contained_symlink_target(
                    relative,
                    target_bytes,
                )
                .is_ok(),
                "variable_artifact_symlink_escape"
            );
            continue;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                if let Some(c) = control {
                    c.remaining(crate::control::AttemptPhase::Build)?;
                }
                pending.push(entry?.path());
            }
        } else {
            ensure!(metadata.is_file(), "variable_artifact_scan_file_type");
            bytes = bytes.saturating_add(metadata.len());
            ensure!(bytes <= 1024 * 1024 * 1024, "variable_artifact_scan_bound");
            let mut file = std::fs::File::open(path)?;
            let mut buffer = vec![0u8; 65536 + overlap];
            let mut retained = 0;
            loop {
                if let Some(c) = control {
                    c.remaining(crate::control::AttemptPhase::Build)?;
                }
                let read = file.read(&mut buffer[retained..])?;
                if read == 0 {
                    break;
                }
                let end = retained + read;
                if !ato_materializer_static_web::blob_is_clean(&buffer[..end], secrets) {
                    return Err(ato_materializer_static_web::ArtifactEmbeddingRefused.into());
                }
                retained = overlap.min(end);
                buffer.copy_within(end - retained..end, 0);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn contained_npm_aliases_are_scanned_without_following_links() {
        use std::os::unix::fs::symlink;
        std::fs::create_dir_all(".tmp").unwrap();
        let root = tempfile::tempdir_in(".tmp").unwrap();
        let bin = root.path().join("node_modules/.bin");
        let package = root.path().join("node_modules/knex/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        let cli = package.join("cli.js");
        std::fs::write(&cli, b"public source script").unwrap();
        symlink("../knex/bin/cli.js", bin.join("knex")).unwrap();
        symlink(".", root.path().join("loop")).unwrap();
        assert!(scan_artifact(root.path(), &[b"synthetic-private-value"]).is_ok());
        std::fs::write(&cli, b"synthetic-private-value").unwrap();
        assert!(scan_artifact(root.path(), &[b"synthetic-private-value"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn artifact_links_refuse_escapes_and_protected_target_bytes() {
        use std::os::unix::fs::symlink;
        std::fs::create_dir_all(".tmp").unwrap();
        let root = tempfile::tempdir_in(".tmp").unwrap();
        for (target, expected) in [
            ("/etc/passwd", "variable_artifact_symlink_escape"),
            ("../outside", "variable_artifact_symlink_escape"),
            ("s/s/../..", "variable_artifact_symlink_escape"),
            (
                "synthetic-private-value",
                "secret_artifact_embedding_refused",
            ),
        ] {
            let link = root.path().join("alias");
            symlink(target, &link).unwrap();
            let error = scan_artifact(root.path(), &[b"synthetic-private-value"])
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            std::fs::remove_file(link).unwrap();
        }
    }

    #[test]
    fn expired_artifact_guard_stops_even_without_redeemed_values() {
        let control =
            crate::control::ExecutionControl::new(crate::control::now_ms().saturating_sub(1));
        assert!(
            scan_artifact_controlled(std::path::Path::new("unused"), &[], Some(&control)).is_err()
        );
    }
    #[test]
    fn protected_values_in_artifact_names_are_refused() {
        let root = tempfile::tempdir_in(".tmp").unwrap();
        std::fs::write(
            root.path().join("synthetic-private-filename"),
            b"public bytes",
        )
        .unwrap();
        assert!(scan_artifact(root.path(), &[b"synthetic-private-filename"]).is_err());
    }
    #[test]
    fn process_artifact_secret_is_refused_across_chunk_boundary() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'x'; 65534];
        bytes.extend_from_slice(b"synthetic-secret");
        std::fs::write(root.path().join("output"), bytes).unwrap();
        let error = scan_artifact(root.path(), &[b"synthetic-secret"]).unwrap_err();
        assert!(
            error
                .downcast_ref::<ato_materializer_static_web::ArtifactEmbeddingRefused>()
                .is_some()
        );
        assert_eq!(
            crate::attempt::failure_of(&error).code,
            "secret_artifact_embedding_refused"
        );
    }
    #[test]
    fn public_configuration_embedding_requires_explicit_permission() {
        std::fs::create_dir_all(".tmp").unwrap();
        let root = tempfile::tempdir_in(".tmp").unwrap();
        std::fs::write(root.path().join("client.js"), b"configuration-sample-value").unwrap();
        for allowed in [false, true] {
            let r:VariableRequirement=serde_json::from_value(serde_json::json!({"name":"CLIENT_ORIGIN","kind":"configuration","purpose":"Client build configuration","resource":"client.configuration","operation":"read","phase":"build","secret":false,"temporary":false,"artifact_embedding":allowed})).unwrap();
            let variables = vec![
                ResolvedVariable::new(
                    &r,
                    "formation-variable:test".into(),
                    "configuration-sample-value".into(),
                )
                .unwrap(),
            ];
            let values = artifact_guard_values(&[r], &variables);
            assert_eq!(scan_artifact(root.path(), &values).is_ok(), allowed);
            assert!(scan_artifact(root.path(), &artifact_guard_values(&[], &variables)).is_err());
        }
    }
    #[test]
    fn redeemed_values_are_not_debuggable_and_output_redaction_is_exact() {
        let r:VariableRequirement=serde_json::from_value(serde_json::json!({"name":"JWT_SECRET","kind":"signing_secret","purpose":"Sign local sessions","resource":"session.signing","operation":"execute","phase":"runtime","secret":true,"temporary":true})).unwrap();
        let variable = ResolvedVariable::new(
            &r,
            "formation-variable:test".into(),
            "synthetic-secret-canary".into(),
        )
        .unwrap();
        assert!(!format!("{variable:?}").contains("synthetic-secret-canary"));
        assert_eq!(
            redact("error synthetic-secret-canary details", &[variable]),
            "error <redacted> details"
        );
    }
}
