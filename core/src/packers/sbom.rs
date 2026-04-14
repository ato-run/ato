use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{CapsuleError, Result};

pub const SBOM_PATH: &str = "sbom.spdx.json";
const DEFAULT_REPRO_EPOCH: i64 = 0;

#[derive(Debug, Clone)]
pub struct EmbeddedSbom {
    pub document: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct SbomFileInput {
    pub archive_path: String,
    pub sha256: String,
    pub disk_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct SpdxCreationInfo {
    created: String,
    creators: Vec<String>,
}

#[derive(Serialize)]
struct SpdxFile {
    #[serde(rename = "SPDXID")]
    spdx_id: String,
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "checksums")]
    checksums: Vec<SpdxChecksum>,
}

#[derive(Serialize)]
struct SpdxChecksum {
    #[serde(rename = "algorithm")]
    algorithm: String,
    #[serde(rename = "checksumValue")]
    checksum_value: String,
}

#[derive(Serialize)]
struct SpdxDocument {
    #[serde(rename = "spdxVersion")]
    spdx_version: String,
    #[serde(rename = "dataLicense")]
    data_license: String,
    #[serde(rename = "SPDXID")]
    spdx_id: String,
    name: String,
    #[serde(rename = "documentNamespace")]
    document_namespace: String,
    #[serde(rename = "creationInfo")]
    creation_info: SpdxCreationInfo,
    files: Vec<SpdxFile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    packages: Vec<SpdxPackage>,
}

#[derive(Serialize, Clone)]
struct SpdxPackage {
    #[serde(rename = "SPDXID")]
    spdx_id: String,
    name: String,
    #[serde(rename = "versionInfo", skip_serializing_if = "Option::is_none")]
    version_info: Option<String>,
    #[serde(rename = "downloadLocation")]
    download_location: String,
    #[serde(rename = "filesAnalyzed")]
    files_analyzed: bool,
    #[serde(rename = "licenseConcluded")]
    license_concluded: String,
    #[serde(rename = "licenseDeclared")]
    license_declared: String,
    #[serde(rename = "copyrightText")]
    copyright_text: String,
}

pub fn generate_embedded_sbom(
    capsule_name: &str,
    files: &[(String, PathBuf)],
) -> Result<EmbeddedSbom> {
    let mut inputs = Vec::with_capacity(files.len());
    for (archive_path, disk_path) in files {
        inputs.push(SbomFileInput {
            archive_path: archive_path.clone(),
            sha256: sha256_hex_file(disk_path)?,
            disk_path: Some(disk_path.clone()),
        });
    }
    generate_embedded_sbom_from_inputs(capsule_name, &inputs)
}

pub fn generate_embedded_sbom_from_inputs(
    capsule_name: &str,
    files: &[SbomFileInput],
) -> Result<EmbeddedSbom> {
    let created_at = reproducible_created_at();
    let namespace_suffix = created_at.format("%Y%m%d%H%M%S").to_string();
    let mut sbom_files = Vec::new();
    for file in files {
        sbom_files.push(SpdxFile {
            spdx_id: format!("SPDXRef-File-{}", sanitize_spdx_id(&file.archive_path)),
            file_name: file.archive_path.clone(),
            checksums: vec![SpdxChecksum {
                algorithm: "SHA256".to_string(),
                checksum_value: file.sha256.clone(),
            }],
        });
    }
    sbom_files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    let sbom_packages = collect_lockfile_packages_from_inputs(files);

    let document = SpdxDocument {
        spdx_version: "SPDX-2.3".to_string(),
        data_license: "CC0-1.0".to_string(),
        spdx_id: "SPDXRef-DOCUMENT".to_string(),
        name: format!("{}-sbom", capsule_name),
        document_namespace: format!(
            "https://ato.run/sbom/{}/{}",
            url::form_urlencoded::byte_serialize(capsule_name.as_bytes()).collect::<String>(),
            namespace_suffix
        ),
        creation_info: SpdxCreationInfo {
            created: created_at.to_rfc3339(),
            creators: vec!["Tool: ato-cli".to_string()],
        },
        files: sbom_files,
        packages: sbom_packages,
    };
    let document = serde_json::to_string_pretty(&document)
        .map_err(|e| CapsuleError::Pack(format!("Failed to serialize SBOM: {e}")))?;
    let sha256 = sha256_hex(document.as_bytes());

    Ok(EmbeddedSbom { document, sha256 })
}

pub async fn generate_embedded_sbom_async(
    capsule_name: String,
    files: Vec<(String, PathBuf)>,
) -> Result<EmbeddedSbom> {
    tokio::task::spawn_blocking(move || generate_embedded_sbom(&capsule_name, &files))
        .await
        .map_err(|e| CapsuleError::Pack(format!("SBOM generation task failed: {e}")))?
}

pub async fn generate_embedded_sbom_from_inputs_async(
    capsule_name: String,
    files: Vec<SbomFileInput>,
) -> Result<EmbeddedSbom> {
    tokio::task::spawn_blocking(move || generate_embedded_sbom_from_inputs(&capsule_name, &files))
        .await
        .map_err(|e| CapsuleError::Pack(format!("SBOM generation task failed: {e}")))?
}

pub fn extract_and_verify_embedded_sbom(capsule_path: &Path) -> Result<String> {
    let mut archive = tar::Archive::new(fs::File::open(capsule_path).map_err(CapsuleError::Io)?);
    let mut sbom = None;
    let mut expected_sha = None;

    for entry in archive.entries().map_err(CapsuleError::Io)? {
        let mut entry = entry.map_err(CapsuleError::Io)?;
        let path = entry
            .path()
            .map_err(CapsuleError::Io)?
            .to_string_lossy()
            .to_string();
        if path == SBOM_PATH {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(CapsuleError::Io)?;
            sbom = Some(bytes);
        } else if path == "signature.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(CapsuleError::Io)?;
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| CapsuleError::Pack(format!("Invalid signature.json: {e}")))?;
            expected_sha = parsed
                .get("sbom")
                .and_then(|v| v.get("sha256"))
                .and_then(|v| v.as_str())
                .map(|v| v.to_string());
        }
    }

    let sbom = sbom
        .ok_or_else(|| CapsuleError::Pack("Embedded SBOM file not found in capsule".to_string()))?;
    let expected_sha = expected_sha
        .ok_or_else(|| CapsuleError::Pack("SBOM metadata missing in signature.json".to_string()))?;

    let actual_sha = sha256_hex(&sbom);
    if actual_sha != expected_sha {
        return Err(CapsuleError::Pack(format!(
            "Embedded SBOM hash mismatch: expected {expected_sha}, got {actual_sha}"
        )));
    }

    let text = String::from_utf8(sbom)
        .map_err(|e| CapsuleError::Pack(format!("Embedded SBOM is not UTF-8: {e}")))?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| CapsuleError::Pack(format!("Embedded SBOM is not valid JSON: {e}")))?;
    Ok(text)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn sha256_hex_file(path: &Path) -> Result<String> {
    let file = fs::File::open(path).map_err(CapsuleError::Io)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buf).map_err(CapsuleError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn reproducible_created_at() -> chrono::DateTime<Utc> {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_REPRO_EPOCH);
    chrono::DateTime::<Utc>::from_timestamp(epoch, 0)
        .unwrap_or_else(|| chrono::DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch"))
}

fn collect_lockfile_packages_from_inputs(files: &[SbomFileInput]) -> Vec<SpdxPackage> {
    let mut packages = BTreeMap::<String, SpdxPackage>::new();
    for file in files {
        let Some(disk_path) = file.disk_path.as_deref() else {
            continue;
        };
        let file_name = file
            .archive_path
            .rsplit('/')
            .next()
            .unwrap_or(file.archive_path.as_str());
        match file_name {
            "package-lock.json" => parse_package_lock(disk_path, &mut packages),
            "deno.lock" => parse_deno_lock(disk_path, &mut packages),
            "uv.lock" => parse_uv_lock(disk_path, &mut packages),
            _ => {}
        }
    }
    packages.into_values().collect()
}

fn parse_package_lock(path: &Path, packages: &mut BTreeMap<String, SpdxPackage>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };

    if let Some(obj) = json.get("packages").and_then(|v| v.as_object()) {
        for (lock_path, value) in obj {
            let version = value.get("version").and_then(|v| v.as_str());
            let explicit_name = value.get("name").and_then(|v| v.as_str());
            let inferred_name = package_name_from_lock_path(lock_path);
            if let (Some(name), Some(version)) =
                (explicit_name.or(inferred_name.as_deref()), version)
            {
                insert_package(packages, name, version);
            }
        }
    }
}

fn parse_deno_lock(path: &Path, packages: &mut BTreeMap<String, SpdxPackage>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(lock_packages) = json.get("packages").and_then(|v| v.as_object()) else {
        return;
    };
    for key in ["npm", "jsr"] {
        let Some(ecosystem) = lock_packages.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for package_key in ecosystem.keys() {
            if let Some((name, version)) = parse_name_version_from_lock_key(package_key) {
                insert_package(packages, &name, &version);
            }
        }
    }
}

fn parse_uv_lock(path: &Path, packages: &mut BTreeMap<String, SpdxPackage>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(doc) = text.parse::<toml::Value>() else {
        return;
    };
    let Some(entries) = doc.get("package").and_then(|v| v.as_array()) else {
        return;
    };
    for entry in entries {
        let name = entry.get("name").and_then(|v| v.as_str());
        let version = entry.get("version").and_then(|v| v.as_str());
        if let (Some(name), Some(version)) = (name, version) {
            insert_package(packages, name, version);
        }
    }
}

fn insert_package(packages: &mut BTreeMap<String, SpdxPackage>, name: &str, version: &str) {
    let name = name.trim();
    let version = version.trim();
    if name.is_empty() || version.is_empty() {
        return;
    }
    let key = format!("{name}@{version}");
    packages.entry(key).or_insert_with(|| SpdxPackage {
        spdx_id: format!(
            "SPDXRef-Package-{}",
            sanitize_spdx_id(&format!("{name}-{version}"))
        ),
        name: name.to_string(),
        version_info: Some(version.to_string()),
        download_location: "NOASSERTION".to_string(),
        files_analyzed: false,
        license_concluded: "NOASSERTION".to_string(),
        license_declared: "NOASSERTION".to_string(),
        copyright_text: "NOASSERTION".to_string(),
    });
}

fn package_name_from_lock_path(lock_path: &str) -> Option<String> {
    if lock_path.is_empty() {
        return None;
    }
    let segment = lock_path.rsplit("node_modules/").next()?;
    let mut parts = segment.split('/');
    let first = parts.next()?;
    if first.starts_with('@') {
        let second = parts.next()?;
        Some(format!("{first}/{second}"))
    } else {
        Some(first.to_string())
    }
}

fn parse_name_version_from_lock_key(raw_key: &str) -> Option<(String, String)> {
    let key = raw_key
        .trim_start_matches("npm:")
        .trim_start_matches("jsr:")
        .trim();
    let split = key.rfind('@')?;
    if split == 0 || split + 1 >= key.len() {
        return None;
    }
    let (name, version) = key.split_at(split);
    Some((
        name.to_string(),
        version.trim_start_matches('@').to_string(),
    ))
}

fn sanitize_spdx_id(path: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in path.chars() {
        let next = if c.is_ascii_alphanumeric() || c == '.' {
            prev_dash = false;
            c
        } else if prev_dash {
            continue;
        } else {
            prev_dash = true;
            '-'
        };
        out.push(next);
    }
    if out.trim_matches('-').is_empty() {
        "file".to_string()
    } else {
        out.trim_matches('-').to_string()
    }
}

#[cfg(test)]
mod tests {
    // std
    use std::fs;
    use std::path::PathBuf;

    // external crates
    use serde_json;
    use tar::Builder;
    use tempfile;

    // internal crates
    use super::{
        extract_and_verify_embedded_sbom, generate_embedded_sbom,
        generate_embedded_sbom_from_inputs, sha256_hex, SbomFileInput, SBOM_PATH,
    };

    #[test]
    fn sbom_generation_fails_closed_for_missing_files() {
        let result = generate_embedded_sbom(
            "demo",
            &[(
                "source/missing.txt".to_string(),
                PathBuf::from("/definitely/missing/file.txt"),
            )],
        );
        assert!(result.is_err());
    }

    #[test]
    fn sbom_from_inputs_uses_prehashed_checksum_without_reread() {
        let sbom = generate_embedded_sbom_from_inputs(
            "demo",
            &[SbomFileInput {
                archive_path: "source/a.txt".to_string(),
                sha256: "deadbeef".to_string(),
                disk_path: None,
            }],
        )
        .expect("sbom");

        let parsed: serde_json::Value = serde_json::from_str(&sbom.document).expect("json");
        let checksum = parsed["files"][0]["checksums"][0]["checksumValue"]
            .as_str()
            .expect("checksum");
        assert_eq!(checksum, "deadbeef");
    }

    #[test]
    fn embedded_sbom_can_be_extracted_and_verified() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let capsule_path = tmp.path().join("demo.capsule");
        let sbom_text = r#"{"spdxVersion":"SPDX-2.3","files":[]}"#;
        let sbom_sha = sha256_hex(sbom_text.as_bytes());
        let signature = serde_json::json!({
            "signed": false,
            "sbom": {
                "path": SBOM_PATH,
                "sha256": sbom_sha,
                "format": "spdx-json"
            }
        });
        let signature_text = signature.to_string();

        let mut file = fs::File::create(&capsule_path).expect("create capsule");
        let mut ar = Builder::new(&mut file);
        let mut sig_header = tar::Header::new_gnu();
        sig_header.set_size(signature_text.len() as u64);
        sig_header.set_mode(0o644);
        sig_header.set_cksum();
        ar.append_data(&mut sig_header, "signature.json", signature_text.as_bytes())
            .expect("append signature");

        let mut sbom_header = tar::Header::new_gnu();
        sbom_header.set_size(sbom_text.len() as u64);
        sbom_header.set_mode(0o644);
        sbom_header.set_cksum();
        ar.append_data(&mut sbom_header, SBOM_PATH, sbom_text.as_bytes())
            .expect("append sbom");
        ar.finish().expect("finish");
        drop(ar);

        let extracted = extract_and_verify_embedded_sbom(&capsule_path).expect("extract");
        let parsed: serde_json::Value = serde_json::from_str(&extracted).expect("json");
        assert_eq!(parsed["spdxVersion"], "SPDX-2.3");
    }

    #[test]
    fn sbom_includes_lockfile_packages_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg_lock = tmp.path().join("package-lock.json");
        let deno_lock = tmp.path().join("deno.lock");
        let uv_lock = tmp.path().join("uv.lock");
        fs::write(
            &pkg_lock,
            r#"{"packages":{"":{"name":"demo","version":"0.1.0"},"node_modules/lodash":{"version":"4.17.21"}}}"#,
        )
        .expect("write package-lock");
        fs::write(
            &deno_lock,
            r#"{"version":"5","packages":{"npm":{"chalk@5.3.0":{"integrity":"x"}}}}"#,
        )
        .expect("write deno.lock");
        fs::write(
            &uv_lock,
            r#"[[package]]
name = "requests"
version = "2.32.3"
"#,
        )
        .expect("write uv.lock");

        let sbom = generate_embedded_sbom(
            "demo",
            &[
                ("source/package-lock.json".to_string(), pkg_lock),
                ("source/deno.lock".to_string(), deno_lock),
                ("source/uv.lock".to_string(), uv_lock),
            ],
        )
        .expect("sbom");
        let parsed: serde_json::Value = serde_json::from_str(&sbom.document).expect("json");
        let packages = parsed
            .get("packages")
            .and_then(|v| v.as_array())
            .expect("packages");
        let names: Vec<_> = packages
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"lodash"));
        assert!(names.contains(&"chalk"));
        assert!(names.contains(&"requests"));
    }
}
