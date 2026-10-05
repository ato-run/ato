//! Owner-private evidence from exact registered operations; no application rules.
use anyhow::{Context, Result, ensure};
use ato_formation::{build_record::*, proposal::python_build_outputs, retained::content_ref};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

pub struct PreparedBuildRecord {
    pub manifest: BuildRecordManifest,
    pub chunks: BTreeMap<String, Vec<u8>>,
}

pub(crate) struct ReadyBuildRecord {
    manifest: BuildRecordManifest,
}
impl ReadyBuildRecord {
    pub fn reference(&self) -> Result<String> {
        Ok(self.manifest.record_ref()?)
    }
    pub fn stored_bytes(&self) -> Result<u64> {
        Ok(self.manifest.stored_bytes()?)
    }
    pub fn roots(&self) -> BTreeSet<PathBuf> {
        self.manifest
            .roots
            .iter()
            .map(|r| PathBuf::from(&r.path))
            .collect()
    }
    pub fn verify_workspace(&self, workspace: &Path) -> Result<()> {
        for file in &self.manifest.files {
            let bytes = regular_file(workspace, &file.path)?;
            ensure!(
                bytes.len() as u64 == file.bytes && content_ref(&bytes) == file.content_ref,
                "build_record_artifact_changed"
            );
        }
        for root in &self.manifest.roots {
            let mut actual = BTreeSet::new();
            let path = workspace.join(&root.path);
            ensure!(
                !std::fs::symlink_metadata(&path)?.is_symlink(),
                "build_record_symlink_refused"
            );
            collect_paths(workspace, &path, &mut actual)?;
            let expected = self
                .manifest
                .files
                .iter()
                .filter(|f| f.path.starts_with(&format!("{}/", root.path)))
                .map(|f| f.path.clone())
                .collect();
            ensure!(actual == expected, "build_record_output_set_changed");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    file: String,
    sha256: String,
    bytes: u64,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Wheel {
    file: String,
    name: String,
    version: String,
    sha256: String,
    bytes: u64,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WheelSet {
    schema: String,
    artifacts: Vec<Wheel>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedInput {
    file: String,
    sha256: String,
    bytes: u64,
    retained_path: String,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Tool {
    name: String,
    version: String,
    executable_sha256: String,
    #[serde(default)]
    cxx_sha256: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonProvenance {
    schema: String,
    requirements_sha256: String,
    acquired: Vec<Artifact>,
    build_dependencies: WheelSet,
    wheels: WheelSet,
    toolchains: Vec<Tool>,
    build_network: String,
    metadata_network: String,
    build_isolation: String,
    retained_inputs: Vec<RetainedInput>,
}
struct ExpectedFile {
    digest: Option<String>,
    bytes: Option<u64>,
    package: Option<String>,
    version: Option<String>,
}
impl ExpectedFile {
    fn evidence() -> Self {
        Self {
            digest: None,
            bytes: None,
            package: None,
            version: None,
        }
    }
}
fn basename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_file(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        ato_formation::proposal::source_path(relative),
        "build_record_path_invalid"
    );
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        path.push(component);
        ensure!(
            !std::fs::symlink_metadata(&path)?.is_symlink(),
            "build_record_symlink_refused"
        );
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "build_record_file_invalid"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&path)?;
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == metadata.len(),
        "build_record_file_changed"
    );
    Ok(bytes)
}
fn regular_file(root: &Path, relative: &str) -> Result<Vec<u8>> {
    read_file(root, relative, MAX_RECORD_BYTES)
}
fn collect_paths(root: &Path, directory: &Path, found: &mut BTreeSet<String>) -> Result<()> {
    ensure!(found.len() <= 1024, "build_record_file_limit");
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(!metadata.is_symlink(), "build_record_symlink_refused");
        if metadata.is_dir() {
            ensure!(
                path.strip_prefix(root)?.components().count() <= 3,
                "build_record_directory_invalid"
            );
            collect_paths(root, &path, found)?;
        } else {
            ensure!(metadata.is_file(), "build_record_file_invalid");
            // Manifest paths use '/' on every host. Preserve each filename;
            // replacing backslashes would alias a literal Unix filename.
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .context("build_record_path_invalid")
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            ensure!(relative.len() <= 512, "build_record_path_invalid");
            ensure!(
                ato_formation::proposal::source_path(&relative),
                "build_record_path_invalid"
            );
            found.insert(relative);
        }
    }
    Ok(())
}

fn wheels(
    root: &str,
    directory: &str,
    set: &WheelSet,
    files: &mut BTreeMap<String, ExpectedFile>,
) -> Result<()> {
    ensure!(
        set.schema == "ato.python-dependency-artifacts/1"
            && !set.artifacts.is_empty()
            && set.artifacts.len() <= 512,
        "build_record_wheel_manifest_invalid"
    );
    let mut packages = BTreeSet::new();
    for wheel in &set.artifacts {
        ensure!(
            basename(&wheel.file)
                && wheel.file.ends_with(".whl")
                && hex(&wheel.sha256)
                && packages.insert(wheel.name.to_ascii_lowercase().replace('_', "-")),
            "build_record_wheel_identity_invalid"
        );
        ensure!(
            files
                .insert(
                    format!("{root}/{directory}/{}", wheel.file),
                    ExpectedFile {
                        digest: Some(format!("sha256:{}", wheel.sha256)),
                        bytes: Some(wheel.bytes),
                        package: Some(wheel.name.clone()),
                        version: Some(wheel.version.clone()),
                    }
                )
                .is_none(),
            "build_record_path_conflict"
        );
    }
    Ok(())
}

fn metadata<T: serde::de::DeserializeOwned>(workspace: &Path, path: &str) -> Result<T> {
    let bytes = read_file(workspace, path, MAX_MANIFEST_BYTES as u64)?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("build_record_provenance_invalid"))
}
fn locked_wheels(workspace: &Path, root: &str, directory: &str, expected: &WheelSet) -> Result<()> {
    let actual: WheelSet = metadata(workspace, &format!("{root}/{directory}/artifacts.json"))?;
    ensure!(&actual == expected, "build_record_lineage_invalid");
    let lock = expected
        .artifacts
        .iter()
        .map(|w| format!("{}=={} --hash=sha256:{}\n", w.name, w.version, w.sha256))
        .collect::<String>();
    ensure!(
        read_file(
            workspace,
            &format!("{root}/{directory}/requirements.lock"),
            MAX_MANIFEST_BYTES as u64
        )? == lock.as_bytes(),
        "build_record_lock_changed"
    );
    Ok(())
}

pub fn prepare(
    workspace: &Path,
    derivation: &ato_formation::authoring::BoundDerivation,
    contract_ref: &str,
    attempt_id: &str,
) -> Result<Option<PreparedBuildRecord>> {
    let outputs = python_build_outputs(derivation)?;
    if outputs.is_empty() {
        return Ok(None);
    }
    let source = derivation
        .inputs
        .iter()
        .find(|i| i.protocol == "ato.workspace@1")
        .context("build_record_source_missing")?;
    let mut roots = Vec::new();
    let mut files = BTreeMap::new();
    for output in outputs {
        let root = &output.root;
        let provenance_bytes = read_file(
            workspace,
            &format!("{root}/provenance.json"),
            MAX_MANIFEST_BYTES as u64,
        )?;
        let provenance: PythonProvenance = serde_json::from_slice(&provenance_bytes)
            .map_err(|_| anyhow::anyhow!("build_record_provenance_invalid"))?;
        ensure!(
            provenance.schema == "ato.python-wheel-build/1"
                && provenance.requirements_sha256 == output.requirements_ref
                && matches!(provenance.build_network.as_str(), "denied" | "scoped-build")
                && provenance.metadata_network == "scoped-dependencies"
                && provenance.build_isolation == "Runtime sandbox and dedicated build-env"
                && !provenance.toolchains.is_empty()
                && provenance.toolchains.len() <= 5
                && provenance.toolchains.iter().all(|t| basename(&t.name)
                    && basename(&t.version)
                    && hex(&t.executable_sha256)
                    && t.cxx_sha256.as_ref().is_none_or(|h| hex(h))),
            "build_record_provenance_invalid"
        );
        let mut tool_names = BTreeSet::new();
        ensure!(
            provenance.build_network == output.build_network
                && provenance
                    .toolchains
                    .iter()
                    .all(|t| tool_names.insert(&t.name))
                && provenance
                    .toolchains
                    .iter()
                    .any(|t| t.name == "build_python" && t.version == output.python_version)
                && output.toolchains.iter().all(|required| provenance
                    .toolchains
                    .iter()
                    .any(|t| t.name == required.name && t.version == required.version))
                && output.build_dependencies.iter().all(|required| provenance
                    .build_dependencies
                    .artifacts
                    .iter()
                    .any(|w| w.name.to_ascii_lowercase().replace('_', "-")
                        == required.name.to_ascii_lowercase().replace('_', "-")
                        && w.version == required.version)),
            "build_record_plan_lineage_invalid"
        );
        ensure!(
            metadata::<Vec<Artifact>>(workspace, &format!("{root}/acquired.json"))?
                == provenance.acquired
                && metadata::<Vec<Tool>>(workspace, &format!("{root}/toolchains.json"))?
                    == provenance.toolchains,
            "build_record_lineage_invalid"
        );
        locked_wheels(
            workspace,
            root,
            "build-dependencies",
            &provenance.build_dependencies,
        )?;
        locked_wheels(workspace, root, "wheels", &provenance.wheels)?;
        for file in [
            "provenance.json",
            "acquired.json",
            "toolchains.json",
            "build-dependencies/artifacts.json",
            "build-dependencies/requirements.lock",
            "wheels/artifacts.json",
            "wheels/requirements.lock",
        ] {
            files.insert(format!("{root}/{file}"), ExpectedFile::evidence());
        }
        wheels(
            root,
            "build-dependencies",
            &provenance.build_dependencies,
            &mut files,
        )?;
        wheels(root, "wheels", &provenance.wheels, &mut files)?;
        ensure!(
            provenance.acquired.len() == provenance.retained_inputs.len()
                && !provenance.acquired.is_empty()
                && provenance.acquired.len() <= 512,
            "build_record_lineage_invalid"
        );
        for (input, retained) in provenance.acquired.iter().zip(&provenance.retained_inputs) {
            ensure!(
                basename(&input.file)
                    && hex(&input.sha256)
                    && input.file == retained.file
                    && input.sha256 == retained.sha256
                    && input.bytes == retained.bytes,
                "build_record_lineage_invalid"
            );
            let path = format!("{root}/{}", retained.retained_path);
            if retained.retained_path == format!("wheels/{}", input.file) {
                let wheel = files.get(&path).context("build_record_input_missing")?;
                ensure!(
                    wheel.digest.as_deref() == Some(&format!("sha256:{}", input.sha256))
                        && wheel.bytes == Some(input.bytes),
                    "build_record_lineage_invalid"
                );
            } else {
                ensure!(
                    retained.retained_path == format!("acquired/{}", input.file),
                    "build_record_lineage_invalid"
                );
                ensure!(
                    files
                        .insert(
                            path,
                            ExpectedFile {
                                digest: Some(format!("sha256:{}", input.sha256)),
                                bytes: Some(input.bytes),
                                package: None,
                                version: None
                            }
                        )
                        .is_none(),
                    "build_record_path_conflict"
                );
            }
        }
        let mut actual = BTreeSet::new();
        // Check the complete registered root; unexpected files are not projected away.
        let root_path = workspace.join(root);
        ensure!(
            !std::fs::symlink_metadata(&root_path)?.is_symlink(),
            "build_record_symlink_refused"
        );
        collect_paths(workspace, &root_path, &mut actual)?;
        let expected = files
            .keys()
            .filter(|p| p.starts_with(&format!("{root}/")))
            .cloned()
            .collect();
        ensure!(actual == expected, "build_record_output_set_changed");
        roots.push(EvidenceRoot {
            path: output.root,
            requirements_ref: output.requirements_ref,
            plan_ref: output.plan_ref,
            provenance_ref: content_ref(&provenance_bytes),
        });
    }
    let mut chunks = BTreeMap::new();
    let mut manifest_files = Vec::new();
    for (path, expected) in files {
        let bytes = regular_file(workspace, &path)?;
        let reference = content_ref(&bytes);
        ensure!(
            expected.digest.as_ref().is_none_or(|r| r == &reference)
                && expected.bytes.is_none_or(|n| n == bytes.len() as u64),
            "build_record_artifact_changed"
        );
        let mut file_chunks = Vec::new();
        for chunk in bytes.chunks(CHUNK_BYTES as usize) {
            let reference = content_ref(chunk);
            if !chunks.contains_key(&reference) {
                let stored: u64 = chunks.values().map(|c: &Vec<u8>| c.len() as u64).sum();
                ensure!(
                    stored.saturating_add(chunk.len() as u64) <= MAX_RECORD_BYTES,
                    "build_record_byte_limit"
                );
                chunks.insert(reference.clone(), chunk.to_vec());
            }
            file_chunks.push(EvidenceChunk {
                content_ref: reference,
                bytes: chunk.len() as u64,
            });
        }
        manifest_files.push(EvidenceFile {
            path,
            content_ref: reference,
            bytes: bytes.len() as u64,
            chunks: file_chunks,
            package: expected.package,
            version: expected.version,
        });
    }
    roots.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = BuildRecordManifest {
        schema: BUILD_RECORD_SCHEMA.into(),
        creation_attempt_id: attempt_id.into(),
        contract_ref: contract_ref.into(),
        derivation_ref: derivation.derivation_ref()?,
        source_closure_ref: source.content_ref.clone(),
        roots,
        files: manifest_files,
    };
    manifest.match_assignment(contract_ref, derivation, &source.content_ref, attempt_id)?;
    manifest.canonical_bytes()?;
    Ok(Some(PreparedBuildRecord { manifest, chunks }))
}

impl PreparedBuildRecord {
    pub(crate) fn acknowledge(&self, reply: &serde_json::Value) -> Result<ReadyBuildRecord> {
        ensure!(
            reply["status"] == "ready"
                && reply["record_ref"] == self.manifest.record_ref()?
                && reply["bytes"].as_u64() == Some(self.manifest.stored_bytes()?),
            "build_record_not_ready"
        );
        Ok(ReadyBuildRecord {
            manifest: self.manifest.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::authoring::BoundDerivation;
    use serde_json::json;
    const ROOT: &str = ".ato-dependencies/python-build-0";
    fn write(root: &Path, path: &str, bytes: &[u8]) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn fixture() -> (tempfile::TempDir, BoundDerivation) {
        std::fs::create_dir_all(".tmp").unwrap();
        let workspace = tempfile::tempdir_in(".tmp").unwrap();
        let requirement = content_ref(b"native==1.0.0");
        let plan = json!({"schema":"ato.python-build-plan/1","python_version":"3.12.7",
            "requirements":"/app/requirements.txt","requirements_sha256":requirement,
            "root":format!("/app/{ROOT}"),"build_dependencies":[{"name":"backend","version":"1.0.0"}],
            "toolchains":[{"name":"gcc","version":"13.3.0"}],"build_network":"denied",
            "lock_operation":include_str!("../../../lib/formation/src/proposal/python-lock-operation.py")});
        let operation =
            include_str!("../../../lib/formation/src/proposal/python-native-operation.py");
        let d: BoundDerivation = serde_json::from_value(json!({"schema":"ato.derivation/1",
            "inputs":[{"id":"workspace","protocol":"ato.workspace@1","content_ref":content_ref(b"source")}],
            "steps":[{"id":"check","protocol":"ato.process@1","op":"exec","argv":["python","-I","-c",operation,"check",plan.to_string()]},
                {"id":"cleanup","protocol":"ato.process@1","op":"exec","argv":["python","-I","-c",operation,"cleanup",plan.to_string()]}],
            "ports":[],"state":[],"effects":"pure"})).unwrap();
        let backend = b"backend wheel";
        let wheel = b"completed native wheel";
        let sdist = b"original sdist";
        let backend_set = json!({"schema":"ato.python-dependency-artifacts/1","artifacts":[{
            "file":"backend.whl","name":"backend","version":"1.0.0","sha256":&content_ref(backend)[7..],"bytes":backend.len()}]});
        let wheel_set = json!({"schema":"ato.python-dependency-artifacts/1","artifacts":[{
            "file":"native.whl","name":"native","version":"1.0.0","sha256":&content_ref(wheel)[7..],"bytes":wheel.len()}]});
        let acquired =
            json!([{"file":"native.tar.gz","sha256":&content_ref(sdist)[7..],"bytes":sdist.len()}]);
        let tools = json!([{"name":"build_python","version":"3.12.7","executable_sha256":"b".repeat(64)}, {"name":"gcc","version":"13.3.0","executable_sha256":"a".repeat(64)}]);
        let provenance = json!({"schema":"ato.python-wheel-build/1","requirements_sha256":requirement,
            "acquired":acquired,"build_dependencies":backend_set,"wheels":wheel_set,"toolchains":tools,
            "build_network":"denied","metadata_network":"scoped-dependencies","build_isolation":"Runtime sandbox and dedicated build-env",
            "retained_inputs":[{"file":"native.tar.gz","sha256":&content_ref(sdist)[7..],"bytes":sdist.len(),"retained_path":"acquired/native.tar.gz"}]});
        for (path, value) in [
            ("provenance.json", provenance),
            ("acquired.json", acquired),
            ("toolchains.json", tools),
            ("build-dependencies/artifacts.json", backend_set),
            ("wheels/artifacts.json", wheel_set),
        ] {
            write(
                workspace.path(),
                &format!("{ROOT}/{path}"),
                &serde_json::to_vec(&value).unwrap(),
            );
        }
        for (path, bytes) in [
            ("build-dependencies/backend.whl", backend.as_slice()),
            ("wheels/native.whl", wheel.as_slice()),
            ("acquired/native.tar.gz", sdist.as_slice()),
        ] {
            write(workspace.path(), &format!("{ROOT}/{path}"), bytes);
        }
        for (directory, name, bytes) in [
            ("build-dependencies", "backend", backend.as_slice()),
            ("wheels", "native", wheel.as_slice()),
        ] {
            write(
                workspace.path(),
                &format!("{ROOT}/{directory}/requirements.lock"),
                format!("{name}==1.0.0 --hash=sha256:{}\n", &content_ref(bytes)[7..]).as_bytes(),
            );
        }
        write(workspace.path(), "requirements.txt", b"native==1.0.0");
        write(workspace.path(), "server.py", b"source-owned server");
        write(
            workspace.path(),
            ".ato-python/lib/site-packages/native/__init__.py",
            b"installed native",
        );
        (workspace, d)
    }
    fn record_fixture(workspace: &Path, d: &BoundDerivation) -> PreparedBuildRecord {
        prepare(workspace, d, &content_ref(b"K"), "attempt")
            .unwrap()
            .unwrap()
    }
    fn acknowledged(prepared: &PreparedBuildRecord) -> ReadyBuildRecord {
        prepared
            .acknowledge(
                &json!({"status":"ready","record_ref":prepared.manifest.record_ref().unwrap(),
            "bytes":prepared.manifest.stored_bytes().unwrap()}),
            )
            .unwrap()
    }
    #[test]
    fn projection_requires_acknowledged_complete_evidence_and_keeps_source_and_installation() {
        let (workspace, d) = fixture();
        let prepared = record_fixture(workspace.path(), &d);
        assert_eq!(prepared.manifest.files.len(), 10);
        assert!(
            prepared
                .manifest
                .files
                .iter()
                .any(|f| f.path.ends_with("native.tar.gz"))
        );
        assert!(prepared.manifest.files.iter().any(
            |f| f.package.as_deref() == Some("native") && f.version.as_deref() == Some("1.0.0")
        ));
        assert!(prepared.acknowledge(&json!({"status":"pending"})).is_err());
        let ready = acknowledged(&prepared);
        let packed =
            crate::pack::pack_process_artifact_with_record(workspace.path(), &ready).unwrap();
        let paths: Vec<_> = tar::Archive::new(packed.as_slice())
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(paths.contains(&"requirements.txt".into()) && paths.contains(&"server.py".into()));
        assert!(paths.iter().any(|p| p.ends_with("native/__init__.py")));
        assert!(!paths.iter().any(|p| p.starts_with(ROOT)));
        assert!(
            workspace
                .path()
                .join(format!("{ROOT}/acquired/native.tar.gz"))
                .is_file(),
            "original build evidence was not deleted"
        );
        let mut unregistered = d;
        unregistered.steps.clear();
        assert!(
            prepare(
                workspace.path(),
                &unregistered,
                &content_ref(b"K"),
                "attempt"
            )
            .unwrap()
            .is_none()
        );
        assert!(crate::pack::pack_tree(workspace.path()).unwrap().len() > packed.len());
    }
    #[test]
    fn changed_hash_unknown_output_and_lock_lineage_stop_projection() {
        let (workspace, d) = fixture();
        let prepared = record_fixture(workspace.path(), &d);
        let ready = acknowledged(&prepared);
        write(
            workspace.path(),
            &format!("{ROOT}/wheels/native.whl"),
            b"mutated native wheel!!",
        );
        assert!(ready.verify_workspace(workspace.path()).is_err());
        assert!(prepare(workspace.path(), &d, &content_ref(b"K"), "attempt").is_err());
        let (workspace, d) = fixture();
        let ready = acknowledged(&record_fixture(workspace.path(), &d));
        write(
            workspace.path(),
            &format!("{ROOT}/unregistered"),
            b"not owned output",
        );
        assert!(ready.verify_workspace(workspace.path()).is_err());
        assert!(prepare(workspace.path(), &d, &content_ref(b"K"), "attempt").is_err());
        let (workspace, d) = fixture();
        write(
            workspace.path(),
            &format!("{ROOT}/wheels/requirements.lock"),
            b"native==2.0.0\n",
        );
        assert!(prepare(workspace.path(), &d, &content_ref(b"K"), "attempt").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn projection_refuses_runtime_links_into_recorded_evidence() {
        use std::os::unix::fs::symlink;
        let (workspace, d) = fixture();
        let ready = acknowledged(&record_fixture(workspace.path(), &d));
        symlink(
            format!("{ROOT}/wheels/native.whl"),
            workspace.path().join("runtime-link"),
        )
        .unwrap();
        assert!(crate::pack::pack_process_artifact_with_record(workspace.path(), &ready).is_err());
        std::fs::remove_file(workspace.path().join("runtime-link")).unwrap();
        symlink(ROOT, workspace.path().join("indirect")).unwrap();
        symlink(
            "indirect/wheels/native.whl",
            workspace.path().join("runtime-link"),
        )
        .unwrap();
        assert!(crate::pack::pack_process_artifact_with_record(workspace.path(), &ready).is_err());
        std::fs::remove_file(workspace.path().join("runtime-link")).unwrap();
        std::fs::remove_file(workspace.path().join("indirect")).unwrap();
        symlink(
            "requirements.txt",
            workspace.path().join(format!("{ROOT}/symlink")),
        )
        .unwrap();
        assert!(prepare(workspace.path(), &d, &content_ref(b"K"), "attempt").is_err());
    }
}
