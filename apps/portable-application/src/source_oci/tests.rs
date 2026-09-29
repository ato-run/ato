//! Pure materializer tests with a fake builder: no Docker, no network.
use super::*;
use std::cell::RefCell;
use std::io::Write;

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn blob_path(d: &str) -> String {
    format!("blobs/sha256/{}", &d[7..])
}
fn tar_bytes(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, content) in members {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, path, content.as_slice()).unwrap();
    }
    b.into_inner().unwrap()
}

/// A docker-save-shaped archive for one image.
fn image_archive(config: Value) -> (Vec<u8>, String, String) {
    let config = serde_json::to_vec(&config).unwrap();
    let layer = b"layer bytes".to_vec();
    let (cd, ld) = (digest(&config), digest(&layer));
    let manifest = serde_json::to_vec(
        &json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
        "config":{"digest":cd,"size":config.len()},"layers":[{"digest":ld,"size":layer.len()}]}),
    )
    .unwrap();
    let md = digest(&manifest);
    let index = serde_json::to_vec(
        &json!({"schemaVersion":2,"manifests":[{"digest":md,"size":manifest.len()}]}),
    )
    .unwrap();
    let legacy = serde_json::to_vec(
        &json!([{"Config":blob_path(&cd),"RepoTags":null,"Layers":[blob_path(&ld)]}]),
    )
    .unwrap();
    let bytes = tar_bytes(&[
        (
            "oci-layout".into(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("index.json".into(), index),
        ("manifest.json".into(), legacy),
        (blob_path(&md), manifest),
        (blob_path(&cd), config),
        (blob_path(&ld), layer),
    ]);
    (bytes, md, cd)
}
fn image_config(cmd: Value, ports: Value, arch: &str) -> Value {
    json!({"architecture":arch,"os":"linux","config":{"Cmd":cmd,"ExposedPorts":ports,"WorkingDir":"/srv"}})
}

/// A base archive whose OCI index root is an image index blob.
fn base_archive() -> (Vec<u8>, String, String) {
    let (_, _, cd) = image_archive(image_config(json!(["sh"]), json!({}), "amd64"));
    let root = serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[]})).unwrap();
    let rd = digest(&root);
    let index =
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[{"digest":rd}]})).unwrap();
    let legacy =
        serde_json::to_vec(&json!([{"Config":blob_path(&cd),"RepoTags":["base:1"],"Layers":[]}]))
            .unwrap();
    let bytes = tar_bytes(&[
        (
            "oci-layout".into(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("index.json".into(), index),
        ("manifest.json".into(), legacy),
        (blob_path(&rd), root),
    ]);
    (bytes, rd, cd)
}

fn source_archive(dir: &Path, dockerfile: bool) -> (PathBuf, String) {
    let name = if dockerfile {
        "source.tar.gz"
    } else {
        "source-no-dockerfile.tar.gz"
    };
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |path: &str, body: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, path, body).unwrap();
    };
    if dockerfile {
        add("app/Dockerfile", b"FROM base:1@sha256:x\nCMD [\"serve\"]\n");
    }
    add("app/index.html", b"<!doctype html>");
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&raw).unwrap();
    let bytes = gz.finish().unwrap();
    let path = dir.join(name);
    std::fs::write(&path, &bytes).unwrap();
    (path, digest(&bytes))
}

struct Fake {
    store: RefCell<Vec<String>>,
    outcome: BuildOutcome,
    built: Vec<u8>,
    built_id: String,
    calls: RefCell<Vec<String>>,
}
impl OciBuilder for Fake {
    fn identity(&self) -> Result<Value> {
        Ok(json!({"fake":true}))
    }
    fn image_ids(&self) -> Result<Vec<String>> {
        Ok(self.store.borrow().clone())
    }
    fn load(&self, _: &Path) -> Result<()> {
        self.store.borrow_mut().push("base".into());
        Ok(())
    }
    fn tag(&self, image: &str, tag: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("tag {image} {tag}"));
        Ok(())
    }
    fn build(
        &self,
        _: &Path,
        platform: &str,
        named: &[(String, String)],
        _: &str,
        _: Duration,
    ) -> Result<BuildOutcome> {
        self.calls
            .borrow_mut()
            .push(format!("build {platform} {named:?}"));
        if self.outcome == BuildOutcome::Built {
            self.store.borrow_mut().push(self.built_id.clone());
        }
        Ok(self.outcome.clone())
    }
    fn image_id(&self, _: &str) -> Result<String> {
        Ok(self.built_id.clone())
    }
    fn save(&self, _: &str, output: &Path) -> Result<()> {
        std::fs::write(output, &self.built).unwrap();
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        self.calls.borrow_mut().push("clear".into());
        self.store.borrow_mut().clear();
        Ok(())
    }
}
fn fake(config: Value, outcome: BuildOutcome) -> Fake {
    let (built, _, cd) = image_archive(config);
    Fake {
        store: RefCell::new(vec![]),
        outcome,
        built,
        built_id: cd,
        calls: RefCell::new(vec![]),
    }
}

fn request(dir: &Path) -> SourceOciRequest {
    let (source, source_sha) = source_archive(dir, true);
    let (base, root, _) = base_archive();
    let base_path = dir.join("base.tar");
    std::fs::write(&base_path, &base).unwrap();
    SourceOciRequest {
        schema: SOURCE_OCI_REQUEST_SCHEMA.into(),
        title: "Fixture".into(),
        source_archive: source,
        source_archive_sha256: source_sha,
        dockerfile: "Dockerfile".into(),
        platform: "linux/amd64".into(),
        base_images: vec![BaseImageInput {
            reference: format!("base:1@{root}"),
            pinned_digest: root.clone(),
            archive: base_path.clone(),
            archive_sha256: digest(&std::fs::read(&base_path).unwrap()),
        }],
        declared_transport_port: 8080,
        policy: SourceOciPolicy {
            network: "none".into(),
            build_timeout_seconds: 60,
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            memory_bytes: 268435456,
            cpu_limit_millis: 500,
            pids_limit: 128,
        },
    }
}
fn ok_config() -> Value {
    image_config(
        json!([
            "python3",
            "-m",
            "http.server",
            "8080",
            "--directory",
            "/srv"
        ]),
        json!({"8080/tcp":{}}),
        "amd64",
    )
}
fn code(r: Result<Materialized>) -> &'static str {
    r.map(|_| ()).unwrap_err().code
}

#[test]
fn materializes_a_verified_archive_and_an_authored_route_the_existing_packer_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    let b = fake(ok_config(), BuildOutcome::Built);
    let m = materialize(&req, &b, &dir.path().join("out")).unwrap();
    assert!(m.image_reference.starts_with("ato-source/") && m.image_reference.contains("@sha256:"));
    let calls = b.calls.borrow().join("\n");
    assert!(calls.contains(&format!(
        "({:?}, \"docker-image://ato-base/b0:frozen\")",
        req.base_images[0].reference
    )));
    assert!(calls.ends_with("clear") && b.store.borrow().is_empty());
    assert_eq!(m.provenance["outputs"]["image_config"]["cmd"][0], "python3");
    assert_eq!(
        m.provenance["profile_divergences"][0]["kind"],
        "working_dir"
    );
    // The existing ato.capsule/2 packer + portable OCI profile accept the route.
    let src = dir.path().join("authored");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("capsule.toml"), &m.capsule_toml).unwrap();
    std::fs::write(
        src.join(PROVENANCE_FILE),
        serde_json::to_vec_pretty(&m.provenance).unwrap(),
    )
    .unwrap();
    let (_, bundle) = crate::build_authored_bundle_v2(&src).unwrap();
    let routes = crate::validate_all_derivations(&bundle).unwrap();
    assert_eq!(
        crate::oci_images(&routes[0]),
        vec![(m.image_reference.clone(), "linux/amd64".to_owned())]
    );
    // The produced archive passes the existing validator as a bundle object.
    let archive = crate::oci_archive::verify_oci_archive(&PortableOciArchive {
        image: m.image_reference.clone(),
        platform: "linux/amd64".into(),
        bytes: base64::engine::general_purpose::STANDARD.encode(std::fs::read(&m.archive).unwrap()),
    });
    assert!(archive.is_ok());
}

#[test]
fn request_bounds_and_selection() {
    let dir = tempfile::tempdir().unwrap();
    let b = fake(ok_config(), BuildOutcome::Built);
    let mut r = request(dir.path());
    r.policy.network = "dependency-resolution".into();
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o1"))),
        "source_oci_network_unauthorized"
    );
    let mut r = request(dir.path());
    r.dockerfile = "docker/Dockerfile".into();
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o2"))),
        "source_oci_dockerfile_unselected"
    );
    let mut r = request(dir.path());
    r.base_images[0].reference = "base:1 ${TAG}".into();
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o3"))),
        "source_oci_base_reference_invalid"
    );
    let mut r = request(dir.path());
    r.policy.build_timeout_seconds = MAX_BUILD_TIMEOUT_SECONDS + 1;
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o4"))),
        "source_oci_request_invalid"
    );
    let mut r = request(dir.path());
    r.source_archive_sha256 = format!("sha256:{}", "0".repeat(64));
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o5"))),
        "source_oci_source_invalid"
    );
    let (src, sha) = source_archive(dir.path(), false);
    let mut r = request(dir.path());
    r.source_archive = src;
    r.source_archive_sha256 = sha;
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o6"))),
        "source_oci_dockerfile_absent"
    );
}

#[test]
fn production_sockets_are_refused() {
    for host in [
        "unix:///var/run/docker.sock",
        "unix:///run/docker.sock",
        "tcp://127.0.0.1:2375",
        "unix://relative.sock",
        "/tmp/x.sock",
    ] {
        assert_eq!(
            DockerCliBuilder::new("docker".into(), host)
                .err()
                .unwrap()
                .code,
            "source_oci_builder_socket_refused",
            "{host}"
        );
    }
    assert!(DockerCliBuilder::new("docker".into(), "unix:///work/run-1/docker.sock").is_ok());
}

#[test]
fn a_non_empty_store_and_unverified_base_inputs_are_refused_before_any_build() {
    let dir = tempfile::tempdir().unwrap();
    let b = fake(ok_config(), BuildOutcome::Built);
    b.store.borrow_mut().push("someone-elses-image".into());
    assert_eq!(
        code(materialize(
            &request(dir.path()),
            &b,
            &dir.path().join("o1")
        )),
        "source_oci_builder_store_not_empty"
    );
    assert!(b.calls.borrow().is_empty());
    let b = fake(ok_config(), BuildOutcome::Built);
    let mut r = request(dir.path());
    r.base_images[0].archive_sha256 = format!("sha256:{}", "1".repeat(64));
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o2"))),
        "source_oci_base_digest_mismatch"
    );
    let mut r = request(dir.path());
    let other = format!("sha256:{}", "2".repeat(64));
    r.base_images[0].reference = format!("base:1@{other}");
    r.base_images[0].pinned_digest = other;
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o3"))),
        "source_oci_base_digest_mismatch"
    );
    assert!(!b.calls.borrow().iter().any(|c| c.starts_with("build")));
}

#[test]
fn build_failures_and_timeouts_are_typed_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    for (outcome, want) in [
        (
            BuildOutcome::Failed {
                log_tail: "resolve docker.io: no route".into(),
            },
            "source_oci_build_failed",
        ),
        (BuildOutcome::TimedOut, "source_oci_build_timeout"),
    ] {
        let b = fake(ok_config(), outcome);
        let out = dir.path().join(want);
        assert_eq!(code(materialize(&request(dir.path()), &b, &out)), want);
        assert_eq!(b.calls.borrow().last().unwrap(), "clear");
        assert!(b.store.borrow().is_empty());
        assert!(!out.join("image.tar").exists());
    }
}

#[test]
fn artifact_checks_bounds_port_cmd_and_platform() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(dir.path());
    r.policy.max_archive_bytes = 10;
    assert_eq!(
        code(materialize(
            &r,
            &fake(ok_config(), BuildOutcome::Built),
            &dir.path().join("o1")
        )),
        "source_oci_artifact_bounds"
    );
    let r = request(dir.path());
    for item in [
        (
            image_config(json!(["x"]), json!({"80/tcp":{}}), "amd64"),
            "source_oci_port_unmatched",
        ),
        (
            image_config(json!(["x"]), json!({"8080/tcp":{},"9000/tcp":{}}), "amd64"),
            "source_oci_port_unmatched_multi",
        ),
        (
            image_config(json!(["x"]), json!({}), "amd64"),
            "source_oci_port_unmatched_none",
        ),
        (
            image_config(json!([]), json!({"8080/tcp":{}}), "amd64"),
            "source_oci_cmd_absent",
        ),
        (
            image_config(json!(["x"]), json!({"8080/tcp":{}}), "arm64"),
            "source_oci_platform_mismatch",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (n, (config, label)) = item;
        let want = if label.starts_with("source_oci_port_unmatched") {
            "source_oci_port_unmatched"
        } else {
            label
        };
        let b = fake(config, BuildOutcome::Built);
        assert_eq!(
            code(materialize(&r, &b, &dir.path().join(format!("o-{n}")))),
            want,
            "{label}"
        );
        assert!(b.store.borrow().is_empty(), "cleanup after {want}");
    }
    // A saved archive whose config is not the built image is refused.
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.built_id = format!("sha256:{}", "3".repeat(64));
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o9"))),
        "source_oci_artifact_digest_mismatch"
    );
}

#[test]
fn a_tag_reference_is_resolved_only_through_an_explicit_pinned_digest() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(dir.path());
    r.base_images[0].reference = "base:1".into();
    let b = fake(ok_config(), BuildOutcome::Built);
    let m = materialize(&r, &b, &dir.path().join("out")).unwrap();
    assert_eq!(
        m.provenance["inputs"]["base_images"][0]["reference"],
        "base:1"
    );
    assert_eq!(
        m.provenance["inputs"]["base_images"][0]["pinned_digest"],
        r.base_images[0].pinned_digest
    );
    assert!(
        b.calls
            .borrow()
            .iter()
            .any(|c| c.contains("(\"base:1\", \"docker-image://ato-base/b0:frozen\")"))
    );
    // A written digest that differs from the pinned one is refused.
    let mut r = request(dir.path());
    r.base_images[0].pinned_digest = format!("sha256:{}", "4".repeat(64));
    assert_eq!(
        code(materialize(&r, &b, &dir.path().join("o"))),
        "source_oci_base_reference_invalid"
    );
}
