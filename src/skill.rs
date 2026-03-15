use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct GeneratedSkillCapsule {
    _temp_dir: TempDir,
    manifest_path: PathBuf,
}

impl GeneratedSkillCapsule {
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    version: Option<String>,
    runtime: Option<String>,
    driver: Option<String>,
    runtime_version: Option<String>,
    entrypoint: Option<String>,
    permissions: Option<SkillPermissions>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillPermissions {
    network: Option<SkillNetworkPermissions>,
    filesystem: Option<SkillFilesystemPermissions>,
    egress_allow: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillNetworkPermissions {
    allow_hosts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SkillFilesystemPermissions {
    read_only: Option<Vec<String>>,
    read_write: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct ParsedSkill {
    frontmatter: SkillFrontmatter,
    code: String,
    code_language: Option<String>,
}

pub fn materialize_skill_capsule(skill_path: &Path) -> Result<GeneratedSkillCapsule> {
    let raw = fs::read_to_string(skill_path)
        .with_context(|| format!("failed to read SKILL file: {}", skill_path.display()))?;
    let parsed = parse_skill(&raw)?;

    let source_kind = decide_source_kind(&parsed);
    let runtime = parsed
        .frontmatter
        .runtime
        .as_deref()
        .unwrap_or("source")
        .trim()
        .to_string();
    let driver = parsed
        .frontmatter
        .driver
        .as_deref()
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| source_kind.default_driver().to_string());
    let entrypoint = parsed
        .frontmatter
        .entrypoint
        .as_deref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| source_kind.default_entrypoint().to_string());
    let runtime_version = parsed
        .frontmatter
        .runtime_version
        .as_deref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| source_kind.default_runtime_version().to_string());

    let name = parsed
        .frontmatter
        .name
        .as_deref()
        .map(sanitize_manifest_name)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "skill-imported-capsule".to_string());

    let version = parsed
        .frontmatter
        .version
        .as_deref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "0.1.0".to_string());

    let allow_hosts = parsed
        .frontmatter
        .permissions
        .as_ref()
        .and_then(|p| {
            p.network
                .as_ref()
                .and_then(|n| n.allow_hosts.clone())
                .or_else(|| p.egress_allow.clone())
        })
        .unwrap_or_default();

    let read_only = parsed
        .frontmatter
        .permissions
        .as_ref()
        .and_then(|p| p.filesystem.as_ref())
        .and_then(|f| f.read_only.clone())
        .unwrap_or_default();

    let read_write = parsed
        .frontmatter
        .permissions
        .as_ref()
        .and_then(|p| p.filesystem.as_ref())
        .and_then(|f| f.read_write.clone())
        .unwrap_or_default();

    let temp_dir = tempfile::tempdir().context("failed to create temporary skill capsule dir")?;
    let root = temp_dir.path();

    let manifest = render_capsule_manifest(
        &name,
        &version,
        &runtime,
        &driver,
        &runtime_version,
        &entrypoint,
        &allow_hosts,
        &read_only,
        &read_write,
    );

    let manifest_path = root.join("capsule.toml");
    fs::write(&manifest_path, manifest).with_context(|| {
        format!(
            "failed to write generated capsule manifest: {}",
            manifest_path.display()
        )
    })?;
    write_capsule_lock(root, &manifest_path)?;

    let entrypoint_path = root.join(&entrypoint);
    if let Some(parent) = entrypoint_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create entrypoint directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(&entrypoint_path, parsed.code).with_context(|| {
        format!(
            "failed to write generated skill entrypoint: {}",
            entrypoint_path.display()
        )
    })?;

    if driver.eq_ignore_ascii_case("deno") {
        fs::write(root.join("deno.lock"), "{}\n").context("failed to write generated deno.lock")?;
    }

    if driver.eq_ignore_ascii_case("native") && entrypoint.ends_with(".py") {
        fs::write(
            root.join("uv.lock"),
            "version = 1\nrevision = 1\nrequires-python = \">=3.11\"\n",
        )
        .context("failed to write generated uv.lock")?;
    }

    Ok(GeneratedSkillCapsule {
        _temp_dir: temp_dir,
        manifest_path,
    })
}

fn parse_skill(raw: &str) -> Result<ParsedSkill> {
    let (frontmatter, body) = split_frontmatter(raw)?;
    let frontmatter = if let Some(frontmatter) = frontmatter {
        serde_yaml::from_str::<SkillFrontmatter>(&frontmatter)
            .context("failed to parse SKILL.md frontmatter as YAML")?
    } else {
        SkillFrontmatter::default()
    };

    let (code, code_language) = extract_primary_code_block(&body);
    let code = code.unwrap_or_else(|| default_code_for_language(code_language.as_deref()));

    Ok(ParsedSkill {
        frontmatter,
        code,
        code_language,
    })
}

fn split_frontmatter(raw: &str) -> Result<(Option<String>, String)> {
    let normalized = raw.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return Ok((None, normalized));
    }

    let rest = &normalized[4..];
    let Some(end_idx) = rest.find("\n---\n") else {
        anyhow::bail!("invalid SKILL.md frontmatter: missing closing '---'");
    };

    let fm = rest[..end_idx].to_string();
    let body = rest[(end_idx + 5)..].to_string();
    Ok((Some(fm), body))
}

fn extract_primary_code_block(body: &str) -> (Option<String>, Option<String>) {
    let mut in_block = false;
    let mut lang: Option<String> = None;
    let mut lines: Vec<String> = Vec::new();

    for line in body.lines() {
        if !in_block {
            if let Some(rest) = line.strip_prefix("```") {
                in_block = true;
                let token = rest.trim();
                if !token.is_empty() {
                    lang = Some(token.to_string());
                }
            }
            continue;
        }

        if line.trim() == "```" {
            break;
        }
        lines.push(line.to_string());
    }

    if lines.is_empty() {
        return (None, lang.map(|v| normalize_language(&v)));
    }

    (
        Some(lines.join("\n") + "\n"),
        lang.map(|v| normalize_language(&v)),
    )
}

fn normalize_language(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "ts" | "typescript" => "typescript".to_string(),
        "js" | "javascript" => "javascript".to_string(),
        "py" | "python" => "python".to_string(),
        other => other.to_string(),
    }
}

fn default_code_for_language(language: Option<&str>) -> String {
    match language.unwrap_or("typescript") {
        "python" => "print(\"skill mvp executed\")\n".to_string(),
        _ => "console.log(\"skill mvp executed\");\n".to_string(),
    }
}

enum SourceKind {
    Deno,
    NativePython,
}

impl SourceKind {
    fn default_driver(&self) -> &'static str {
        match self {
            SourceKind::Deno => "deno",
            SourceKind::NativePython => "native",
        }
    }

    fn default_entrypoint(&self) -> &'static str {
        match self {
            SourceKind::Deno => "main.ts",
            SourceKind::NativePython => "main.py",
        }
    }

    fn default_runtime_version(&self) -> &'static str {
        match self {
            SourceKind::Deno => "1.46.3",
            SourceKind::NativePython => "3.11.9",
        }
    }
}

fn decide_source_kind(parsed: &ParsedSkill) -> SourceKind {
    if let Some(driver) = parsed.frontmatter.driver.as_deref() {
        if driver.eq_ignore_ascii_case("native") {
            return SourceKind::NativePython;
        }
        if driver.eq_ignore_ascii_case("deno") {
            return SourceKind::Deno;
        }
    }

    match parsed.code_language.as_deref() {
        Some("python") => SourceKind::NativePython,
        _ => SourceKind::Deno,
    }
}

fn sanitize_manifest_name(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn render_capsule_manifest(
    name: &str,
    version: &str,
    runtime: &str,
    driver: &str,
    runtime_version: &str,
    entrypoint: &str,
    allow_hosts: &[String],
    read_only: &[String],
    read_write: &[String],
) -> String {
    let hosts = render_toml_array(allow_hosts);
    let ro = render_toml_array(read_only);
    let rw = render_toml_array(read_write);

    format!(
        "schema_version = \"0.2\"\nname = \"{}\"\nversion = \"{}\"\ntype = \"app\"\ndefault_target = \"cli\"\n\n[network]\negress_allow = {}\n\n[sandbox.filesystem]\nread_only = {}\nread_write = {}\n\n[targets.cli]\nruntime = \"{}\"\ndriver = \"{}\"\nruntime_version = \"{}\"\nentrypoint = \"{}\"\n",
        name, version, hosts, ro, rw, runtime, driver, runtime_version, entrypoint
    )
}

fn render_toml_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }

    let quoted = values
        .iter()
        .map(|v| format!("\"{}\"", v.replace('"', "\\\"")))
        .collect::<Vec<String>>()
        .join(", ");
    format!("[{}]", quoted)
}

fn write_capsule_lock(root: &Path, manifest_path: &Path) -> Result<()> {
    let manifest_text = fs::read_to_string(manifest_path).with_context(|| {
        format!(
            "failed to read generated manifest: {}",
            manifest_path.display()
        )
    })?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_text)
        .context("failed to parse generated manifest schema")?;
    let hash = capsule_core::packers::payload::compute_manifest_hash_without_signatures(&manifest)
        .context("failed to compute generated manifest hash")?;

    let lock = serde_json::json!({
        "version": "1",
        "meta": {
            "created_at": "2026-02-24T00:00:00Z",
            "manifest_hash": hash,
        },
        "targets": {}
    });

    let lock_path = root.join("capsule.lock.json");
    let rendered =
        serde_json::to_vec_pretty(&lock).context("failed to render generated lockfile")?;
    fs::write(&lock_path, rendered).with_context(|| {
        format!(
            "failed to write generated lockfile: {}",
            lock_path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_defaults_to_fail_closed_permissions() {
        let raw = "```ts\nconsole.log('ok')\n```\n";
        let parsed = parse_skill(raw).expect("parse should succeed");
        assert!(parsed.frontmatter.permissions.is_none());
        assert_eq!(parsed.code_language.as_deref(), Some("typescript"));
    }

    #[test]
    fn materialize_skill_generates_manifest_and_deno_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_path = temp.path().join("SKILL.md");
        fs::write(
            &skill_path,
            "---\nname: test-skill\npermissions:\n  network:\n    allow_hosts:\n      - api.example.com\n---\n```ts\nconsole.log('hello')\n```\n",
        )
        .expect("write skill");

        let generated = materialize_skill_capsule(&skill_path).expect("materialize");
        let manifest = fs::read_to_string(generated.manifest_path()).expect("manifest");

        assert!(manifest.contains("name = \"test-skill\""));
        assert!(manifest.contains("egress_allow = [\"api.example.com\"]"));
        assert!(manifest.contains("runtime_version = \"1.46.3\""));
        assert!(generated
            .manifest_path()
            .parent()
            .unwrap()
            .join("deno.lock")
            .exists());
    }
}
