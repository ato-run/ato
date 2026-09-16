//! Python wheel source discovery and digest-checked sparse-bundle hydration.
//! URLs are transport hints; the bundle's object references remain authority.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_objects::{PortableApplicationBundle, PortableBundlePayload, PortableOciArchive};
use ato_portable_application::{
    OCI_IMAGE_RUNTIME, OCI_PLATFORM_RUNTIME, PortableRealizationKind, bundle_sha256,
    oci_archive::verify_oci_archive, validate_all_derivations,
};
use base64::Engine;
use reqwest::Url;
use reqwest::blocking::Client;

fn dependency_client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?)
}

pub fn oci_archive_from_file(
    bundle: &PortableApplicationBundle,
    path: &Path,
) -> Result<Vec<PortableOciArchive>> {
    let images = validate_all_derivations(bundle)?
        .into_iter()
        .filter(|route| route.realization == PortableRealizationKind::OciContainer)
        .map(|route| {
            Ok((
                route
                    .derivation
                    .runtimes
                    .get(OCI_IMAGE_RUNTIME)
                    .context("OCI image missing")?
                    .clone(),
                route
                    .derivation
                    .runtimes
                    .get(OCI_PLATFORM_RUNTIME)
                    .context("OCI platform missing")?
                    .clone(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        images.len() == 1,
        "--oci-archive requires exactly one OCI Derivation"
    );
    let size = std::fs::metadata(path)
        .with_context(|| format!("inspect {}", path.display()))?
        .len();
    ensure!(
        size <= 128 * 1024 * 1024,
        "OCI archive exceeds bounded transport size"
    );
    let archive = PortableOciArchive {
        image: images[0].0.clone(),
        platform: images[0].1.clone(),
        bytes: base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(path).with_context(|| format!("read {}", path.display()))?),
    };
    verify_oci_archive(&archive).context("verify supplied OCI archive")?;
    Ok(vec![archive])
}

fn official_wheel_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("dependency source URL is invalid")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("files.pythonhosted.org")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "dependency source is not an HTTPS PyPI file URL"
    );
    Ok(url)
}

pub fn discover_wheel_sources(
    bundle: &PortableApplicationBundle,
) -> Result<BTreeMap<String, Vec<String>>> {
    let routes = validate_all_derivations(bundle)?;
    let client = dependency_client()?;
    let mut releases = BTreeMap::<(String, String), serde_json::Value>::new();
    let mut sources = bundle
        .portability
        .as_ref()
        .into_iter()
        .flat_map(|portability| &portability.external_objects)
        .map(|external| {
            for source in &external.sources {
                official_wheel_url(source)?;
            }
            Ok((external.reference.clone(), external.sources.clone()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for entry in &routes[0].tree.entries {
        if !entry.path.ends_with(".whl") || sources.contains_key(&entry.content_ref) {
            continue;
        }
        let filename = entry
            .path
            .rsplit('/')
            .next()
            .context("wheel path is empty")?;
        let mut fields = filename.split('-');
        let project = fields.next().context("wheel omits project")?;
        let version = fields.next().context("wheel omits version")?;
        for field in [project, version] {
            ensure!(
                !field.is_empty()
                    && field
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'-' | b'.')),
                "wheel name/version cannot be used for PyPI discovery: {filename}"
            );
        }
        let key = (project.to_owned(), version.to_owned());
        if !releases.contains_key(&key) {
            let endpoint = format!("https://pypi.org/pypi/{project}/{version}/json");
            let response = client
                .get(&endpoint)
                .send()
                .context("query PyPI release metadata")?;
            ensure!(
                response.status().is_success(),
                "PyPI release metadata unavailable for {project} {version}"
            );
            releases.insert(
                key.clone(),
                response.json().context("decode PyPI release metadata")?,
            );
        }
        let files = releases[&key]["urls"]
            .as_array()
            .context("PyPI release metadata has no files")?;
        let digest = entry
            .content_ref
            .strip_prefix("sha256:")
            .context("wheel digest is not SHA-256")?;
        let source = files
            .iter()
            .find(|file| file["filename"] == filename && file["digests"]["sha256"] == digest)
            .and_then(|file| file["url"].as_str())
            .with_context(|| format!("PyPI has no matching file and SHA-256 for {filename}"))?;
        official_wheel_url(source)?;
        sources.insert(entry.content_ref.clone(), vec![source.to_owned()]);
    }
    Ok(sources)
}

pub fn hydrate_external_objects(
    original: &PortableApplicationBundle,
) -> Result<(PortableApplicationBundle, Vec<String>)> {
    let Some(portability) = original.portability.as_ref() else {
        return Ok((original.clone(), Vec::new()));
    };
    if portability.external_objects.is_empty() {
        return Ok((original.clone(), Vec::new()));
    }
    let client = dependency_client()?;
    let mut hydrated = original.clone();
    let mut fetched = Vec::new();
    for external in &portability.external_objects {
        let descriptor = original
            .index
            .objects
            .iter()
            .find(|descriptor| descriptor.reference == external.reference)
            .context("external dependency descriptor is missing")?;
        let mut bytes = None;
        for source in &external.sources {
            let url = official_wheel_url(source)?;
            let Ok(mut response) = client.get(url).send() else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            if response
                .content_length()
                .is_some_and(|length| length > descriptor.size)
            {
                bail!("dependency digest/size failure for {}", external.reference);
            }
            let mut candidate = Vec::new();
            response
                .by_ref()
                .take(descriptor.size + 1)
                .read_to_end(&mut candidate)
                .context("read fetched dependency")?;
            verify_fetched_dependency(&external.reference, descriptor.size, &candidate)?;
            bytes = Some(candidate);
            break;
        }
        let bytes =
            bytes.with_context(|| format!("dependency unavailable: {}", external.reference))?;
        hydrated.payloads.push(PortableBundlePayload {
            reference: external.reference.clone(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        fetched.push(external.reference.clone());
    }
    hydrated
        .payloads
        .sort_by(|left, right| left.reference.cmp(&right.reference));
    hydrated
        .portability
        .as_mut()
        .context("missing portability metadata")?
        .external_objects
        .clear();
    validate_all_derivations(&hydrated)?;
    Ok((hydrated, fetched))
}

fn verify_fetched_dependency(reference: &str, expected_size: u64, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 == expected_size && bundle_sha256(bytes) == reference,
        "dependency digest/size failure for {reference}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetched_bytes_must_match_the_declared_digest_and_size() {
        let expected = bundle_sha256(b"immutable wheel");
        assert!(verify_fetched_dependency(&expected, 15, b"immutable wheel").is_ok());
        assert!(verify_fetched_dependency(&expected, 15, b"modified wheel!").is_err());
        assert!(verify_fetched_dependency(&expected, 14, b"immutable wheel").is_err());
    }

    #[test]
    fn retrieval_url_cannot_redirect_to_an_arbitrary_host() {
        assert!(official_wheel_url("https://files.pythonhosted.org/a.whl").is_ok());
        assert!(official_wheel_url("https://example.org/a.whl").is_err());
        assert!(official_wheel_url("http://files.pythonhosted.org/a.whl").is_err());
    }
}
