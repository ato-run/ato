//! Pure materializer tests with a fake builder: no Docker, no network.
use super::*;
use std::cell::{Cell, RefCell};
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
fn layout() -> (String, Vec<u8>) {
    (
        "oci-layout".into(),
        br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
    )
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
        layout(),
        ("index.json".into(), index),
        ("manifest.json".into(), legacy),
        (blob_path(&md), manifest),
        (blob_path(&cd), config),
        (blob_path(&ld), layer),
        // A classic store also writes an unreferenced legacy v1 layer JSON.
        (
            blob_path(&digest(b"{\"id\":\"legacy\"}")),
            b"{\"id\":\"legacy\"}".to_vec(),
        ),
    ]);
    (bytes, md, cd)
}
fn image_config(cmd: Value, ports: Value, arch: &str) -> Value {
    json!({"architecture":arch,"os":"linux","config":{"Cmd":cmd,"ExposedPorts":ports,"WorkingDir":"/srv"}})
}

/// How a base archive is shaped; the default is a faithful multi-platform
/// `docker save` of one pulled tag.
#[derive(Default, Clone, Copy)]
struct BaseShape {
    /// manifest.json names a config the pinned root never reaches.
    load_config_elsewhere: bool,
    /// A second member with the name of the platform manifest.
    duplicate_member: bool,
    /// The index offers two amd64 manifests.
    ambiguous_platform: bool,
    seed: u8,
}

/// A base archive: index.json -> image index (root) -> amd64 manifest (+ an
/// attestation manifest) -> config + one layer; manifest.json for Docker load.
/// Returns (bytes, root digest, amd64 config digest).
fn base_archive(shape: BaseShape) -> (Vec<u8>, String, String) {
    let config = serde_json::to_vec(
        &json!({"architecture":"amd64","os":"linux","config":{"Cmd":["sh"]},"seed":shape.seed}),
    )
    .unwrap();
    let layer = vec![b'l', shape.seed];
    let (cd, ld) = (digest(&config), digest(&layer));
    let manifest = serde_json::to_vec(&json!({"schemaVersion":2,
        "mediaType":"application/vnd.oci.image.manifest.v1+json",
        "config":{"digest":cd,"size":config.len()},"layers":[{"digest":ld,"size":layer.len()}]}))
    .unwrap();
    let md = digest(&manifest);
    let other_config =
        serde_json::to_vec(&json!({"architecture":"amd64","os":"linux","other":shape.seed}))
            .unwrap();
    let ocd = digest(&other_config);
    let other = serde_json::to_vec(&json!({"schemaVersion":2,
        "config":{"digest":ocd,"size":other_config.len()},"layers":[{"digest":ld,"size":layer.len()}]}))
    .unwrap();
    let omd = digest(&other);
    let mut entries = vec![
        json!({"digest":md,"size":manifest.len(),"platform":{"os":"linux","architecture":"amd64"}}),
        json!({"digest":omd,"size":other.len(),"platform":{"os":"unknown","architecture":"unknown"},
            "annotations":{"vnd.docker.reference.type":"attestation-manifest"}}),
    ];
    if shape.ambiguous_platform {
        entries[1]["platform"] = json!({"os":"linux","architecture":"amd64"});
    }
    let root = serde_json::to_vec(&json!({"schemaVersion":2,
        "mediaType":"application/vnd.oci.image.index.v1+json","manifests":entries}))
    .unwrap();
    let rd = digest(&root);
    let index = serde_json::to_vec(&json!({"schemaVersion":2,
        "manifests":[{"digest":rd,"size":root.len()}]}))
    .unwrap();
    let load_config = if shape.load_config_elsewhere {
        &ocd
    } else {
        &cd
    };
    let legacy = serde_json::to_vec(
        &json!([{"Config":blob_path(load_config),"RepoTags":["base:1"],"Layers":[blob_path(&ld)]}]),
    )
    .unwrap();
    let mut members = vec![
        layout(),
        ("index.json".into(), index),
        ("manifest.json".into(), legacy),
        (blob_path(&rd), root),
        (blob_path(&md), manifest.clone()),
        (blob_path(&cd), config),
        (blob_path(&ld), layer),
        (blob_path(&omd), other),
        (blob_path(&ocd), other_config),
    ];
    if shape.duplicate_member {
        members.push((blob_path(&md), manifest));
    }
    (tar_bytes(&members), rd, cd)
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

enum BuildResult {
    Outcome(BuildOutcome),
    /// The client lost the daemon mid-build.
    Disconnected,
}

struct Fake {
    scratch: tempfile::TempDir,
    store: RefCell<Vec<String>>,
    build: BuildResult,
    built: Vec<u8>,
    built_id: String,
    reported_size: u64,
    fail_tag: Option<usize>,
    fail_release: bool,
    released: Cell<u32>,
    calls: RefCell<Vec<String>>,
}
impl OciBuilder for Fake {
    fn scratch(&self) -> &Path {
        self.scratch.path()
    }
    fn identity(&self) -> Result<Value> {
        Ok(json!({"fake":true}))
    }
    fn image_ids(&self) -> Result<Vec<String>> {
        Ok(self.store.borrow().clone())
    }
    fn load(&self, archive: &Path) -> Result<()> {
        // Docker loads what manifest.json names.
        let members = scan_saved(archive, MAX_BASE_ARCHIVE_BYTES).unwrap();
        let legacy = member_json(&members, "manifest.json", "m").unwrap();
        let config = legacy[0]["Config"]
            .as_str()
            .unwrap()
            .replace("blobs/sha256/", "sha256:");
        self.calls.borrow_mut().push(format!("load {config}"));
        self.store.borrow_mut().push(config);
        Ok(())
    }
    fn tag(&self, image: &str, tag: &str) -> Result<()> {
        let n = self
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("tag"))
            .count();
        self.calls.borrow_mut().push(format!("tag {image} {tag}"));
        if self.fail_tag == Some(n) {
            return Err(err("source_oci_builder_failed", "tag refused"));
        }
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
        match &self.build {
            BuildResult::Disconnected => Err(err(
                "source_oci_builder_unavailable",
                "connection to the private daemon was lost",
            )),
            BuildResult::Outcome(outcome) => {
                if *outcome == BuildOutcome::Built {
                    self.store.borrow_mut().push(self.built_id.clone());
                }
                Ok(outcome.clone())
            }
        }
    }
    fn image_id(&self, _: &str) -> Result<String> {
        Ok(self.built_id.clone())
    }
    fn image_size(&self, _: &str) -> Result<u64> {
        Ok(self.reported_size)
    }
    fn save(&self, _: &str, output: &Path) -> Result<()> {
        self.calls.borrow_mut().push("save".into());
        std::fs::write(output, &self.built).unwrap();
        Ok(())
    }
    fn release(&self) -> Result<()> {
        self.released.set(self.released.get() + 1);
        self.calls.borrow_mut().push("release".into());
        if self.fail_release {
            return Err(err(
                "source_oci_cleanup_unconfirmed",
                "processes remain in the session cgroup",
            ));
        }
        self.store.borrow_mut().clear();
        Ok(())
    }
}
fn fake(config: Value, outcome: BuildOutcome) -> Fake {
    let (built, _, cd) = image_archive(config);
    Fake {
        scratch: tempfile::tempdir().unwrap(),
        store: RefCell::new(vec![]),
        build: BuildResult::Outcome(outcome),
        built,
        built_id: cd,
        reported_size: 64,
        fail_tag: None,
        fail_release: false,
        released: Cell::new(0),
        calls: RefCell::new(vec![]),
    }
}

fn base_input(dir: &Path, name: &str, reference: &str, shape: BaseShape) -> BaseImageInput {
    let (base, root, _) = base_archive(shape);
    let path = dir.join(name);
    std::fs::write(&path, &base).unwrap();
    BaseImageInput {
        reference: format!("{reference}@{root}"),
        pinned_digest: root,
        archive_sha256: digest(&base),
        archive: path,
    }
}

fn request(dir: &Path) -> SourceOciRequest {
    let (source, source_sha) = source_archive(dir, true);
    SourceOciRequest {
        schema: SOURCE_OCI_REQUEST_SCHEMA.into(),
        title: "Fixture".into(),
        source_archive: source,
        source_archive_sha256: source_sha,
        dockerfile: "Dockerfile".into(),
        platform: "linux/amd64".into(),
        base_images: vec![base_input(dir, "base.tar", "base:1", BaseShape::default())],
        declared_transport_port: 8080,
        policy: SourceOciPolicy {
            network: "none".into(),
            build_timeout_seconds: 60,
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            build: BuildLimits {
                memory_bytes: 2 << 30,
                cpu_limit_millis: 2000,
                pids_limit: 512,
                disk_bytes: 4 << 30,
            },
            runtime: RuntimeLimits {
                memory_bytes: 268435456,
                cpu_limit_millis: 500,
                pids_limit: 128,
            },
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
fn run(r: &SourceOciRequest, b: &dyn OciBuilder, out: &Path) -> Result<Materialized> {
    prepare(r).and_then(|p| materialize(&p, b, out))
}
fn code(r: Result<Materialized>) -> &'static str {
    r.map(|_| ()).unwrap_err().code
}

#[test]
fn materializes_a_verified_archive_and_an_authored_route_the_existing_packer_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    let b = fake(ok_config(), BuildOutcome::Built);
    let m = run(&req, &b, &dir.path().join("out")).unwrap();
    assert!(m.image_reference.starts_with("ato-source/") && m.image_reference.contains("@sha256:"));
    let calls = b.calls.borrow().join("\n");
    assert!(calls.contains(&format!(
        "({:?}, \"docker-image://ato-base/b0:frozen\")",
        req.base_images[0].reference
    )));
    assert!(calls.ends_with("release") && b.released.get() == 1 && b.store.borrow().is_empty());
    assert_eq!(m.provenance["outputs"]["image_config"]["cmd"][0], "python3");
    assert_eq!(
        m.provenance["profile_divergences"][0]["kind"],
        "working_dir"
    );
    // The pinned root, the selected platform manifest and the loaded config
    // are recorded separately.
    let base = &m.provenance["inputs"]["base_images"][0];
    assert_eq!(base["pinned_digest"], req.base_images[0].pinned_digest);
    assert_ne!(base["platform_manifest_digest"], base["pinned_digest"]);
    assert_eq!(
        calls.lines().next().unwrap(),
        format!("load {}", base["config_digest"].as_str().unwrap())
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
    assert_eq!(
        m.provenance["outputs"]["repack"]["dropped_unreferenced_members"],
        1
    );
    let out: Vec<_> = std::fs::read_dir(dir.path().join("out"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(out, vec![ARCHIVE_FILE.to_owned()]);
}

#[test]
fn request_bounds_and_selection() {
    let dir = tempfile::tempdir().unwrap();
    let b = fake(ok_config(), BuildOutcome::Built);
    let mut r = request(dir.path());
    r.policy.network = "dependency-resolution".into();
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o1"))),
        "source_oci_network_unauthorized"
    );
    let mut r = request(dir.path());
    r.dockerfile = "docker/Dockerfile".into();
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o2"))),
        "source_oci_dockerfile_unselected"
    );
    let mut r = request(dir.path());
    r.base_images[0].reference = "base:1 ${TAG}".into();
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o3"))),
        "source_oci_base_reference_invalid"
    );
    let mutations: [fn(&mut SourceOciRequest); 6] = [
        |r| r.policy.build_timeout_seconds = MAX_BUILD_TIMEOUT_SECONDS + 1,
        |r| r.policy.build.disk_bytes = MAX_BUILD_DISK_BYTES + 1,
        |r| r.policy.build.disk_bytes = MIN_BUILD_DISK_BYTES - 1,
        |r| r.policy.build.memory_bytes = 0,
        |r| r.policy.build.pids_limit = 1,
        |r| r.policy.build.cpu_limit_millis = 0,
    ];
    for mutate in mutations {
        let mut r = request(dir.path());
        mutate(&mut r);
        assert_eq!(
            code(run(&r, &b, &dir.path().join("o4"))),
            "source_oci_request_invalid"
        );
    }
    let mut r = request(dir.path());
    r.source_archive_sha256 = format!("sha256:{}", "0".repeat(64));
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o5"))),
        "source_oci_source_invalid"
    );
    let (src, sha) = source_archive(dir.path(), false);
    let mut r = request(dir.path());
    r.source_archive = src;
    r.source_archive_sha256 = sha;
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o6"))),
        "source_oci_dockerfile_absent"
    );
    // Only the Dockerfile check needed the builder's scratch; nothing was
    // loaded or built, and the builder was released.
    assert!(b.calls.borrow().iter().all(|c| c == "release"));
}

#[test]
fn a_base_whose_pinned_graph_does_not_reach_the_load_config_is_refused_before_any_load() {
    let dir = tempfile::tempdir().unwrap();
    for (label, shape) in [
        (
            "load config elsewhere",
            BaseShape {
                load_config_elsewhere: true,
                ..Default::default()
            },
        ),
        (
            "duplicate member",
            BaseShape {
                duplicate_member: true,
                ..Default::default()
            },
        ),
        (
            "ambiguous platform",
            BaseShape {
                ambiguous_platform: true,
                ..Default::default()
            },
        ),
    ] {
        let mut r = request(dir.path());
        r.base_images = vec![base_input(dir.path(), "bad.tar", "base:1", shape)];
        let e = prepare(&r).map(|_| ()).unwrap_err();
        assert_eq!(e.code, "source_oci_base_graph_invalid", "{label}: {e}");
    }
    // The frozen archive bytes themselves.
    let mut r = request(dir.path());
    r.base_images[0].archive_sha256 = format!("sha256:{}", "1".repeat(64));
    assert_eq!(
        prepare(&r).map(|_| ()).unwrap_err().code,
        "source_oci_base_digest_mismatch"
    );
    let mut r = request(dir.path());
    let other = format!("sha256:{}", "2".repeat(64));
    r.base_images[0].reference = format!("base:1@{other}");
    r.base_images[0].pinned_digest = other;
    assert_eq!(
        prepare(&r).map(|_| ()).unwrap_err().code,
        "source_oci_base_graph_invalid"
    );
}

#[test]
fn a_failure_after_the_first_load_still_releases_the_builder() {
    let dir = tempfile::tempdir().unwrap();
    let two = |dir: &Path| {
        let mut r = request(dir);
        r.base_images.push(base_input(
            dir,
            "base2.tar",
            "base:2",
            BaseShape {
                seed: 2,
                ..Default::default()
            },
        ));
        r
    };
    // The second base's tag fails after the first base was loaded and tagged.
    let r = two(dir.path());
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.fail_tag = Some(1);
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o1"))),
        "source_oci_builder_failed"
    );
    assert_eq!(b.released.get(), 1);
    assert!(b.store.borrow().is_empty());
    assert!(!dir.path().join("o1").exists());
    // The second base archive changes after it was verified: refused before
    // it is loaded, and the first load is still released.
    let r = two(dir.path());
    let prepared = prepare(&r).unwrap();
    std::fs::write(&r.base_images[1].archive, b"replaced").unwrap();
    let b = fake(ok_config(), BuildOutcome::Built);
    let e = materialize(&prepared, &b, &dir.path().join("o2"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, "source_oci_base_digest_mismatch");
    assert_eq!(
        b.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("load"))
            .count(),
        1
    );
    assert_eq!(b.released.get(), 1);
}

#[test]
fn build_failures_timeouts_log_bounds_and_lost_clients_are_typed_and_released() {
    let dir = tempfile::tempdir().unwrap();
    for (build, want) in [
        (
            BuildResult::Outcome(BuildOutcome::Failed {
                log_tail: "resolve docker.io: no route".into(),
            }),
            "source_oci_build_failed",
        ),
        (
            BuildResult::Outcome(BuildOutcome::TimedOut),
            "source_oci_build_timeout",
        ),
        (
            BuildResult::Outcome(BuildOutcome::LogExceeded),
            "source_oci_log_bound",
        ),
        (BuildResult::Disconnected, "source_oci_builder_unavailable"),
    ] {
        let mut b = fake(ok_config(), BuildOutcome::Built);
        b.build = build;
        let out = dir.path().join(want);
        assert_eq!(code(run(&request(dir.path()), &b, &out)), want);
        assert_eq!(b.calls.borrow().last().unwrap(), "release");
        assert_eq!(b.released.get(), 1);
        assert!(b.store.borrow().is_empty());
        assert!(!b.calls.borrow().iter().any(|c| c == "save"));
        assert!(!out.exists(), "nothing is published for {want}");
    }
}

#[test]
fn an_unconfirmed_release_is_kept_with_the_original_error_and_nothing_is_published() {
    let dir = tempfile::tempdir().unwrap();
    // Original failure + release failure: both are returned.
    let mut b = fake(ok_config(), BuildOutcome::TimedOut);
    b.fail_release = true;
    let e = run(&request(dir.path()), &b, &dir.path().join("o1"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, "source_oci_build_timeout");
    assert_eq!(
        e.cleanup.as_ref().unwrap().code,
        "source_oci_cleanup_unconfirmed"
    );
    assert!(
        e.to_string()
            .contains("cleanup: source_oci_cleanup_unconfirmed")
    );
    assert!(!dir.path().join("o1").exists());
    // A successful build whose release is unconfirmed is not a success.
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.fail_release = true;
    assert_eq!(
        code(run(&request(dir.path()), &b, &dir.path().join("o2"))),
        "source_oci_cleanup_unconfirmed"
    );
    assert!(!dir.path().join("o2").exists());
}

#[test]
fn a_non_empty_store_is_refused_before_any_load() {
    let dir = tempfile::tempdir().unwrap();
    let b = fake(ok_config(), BuildOutcome::Built);
    b.store.borrow_mut().push("someone-elses-image".into());
    assert_eq!(
        code(run(&request(dir.path()), &b, &dir.path().join("o1"))),
        "source_oci_builder_store_not_empty"
    );
    assert_eq!(*b.calls.borrow(), vec!["release".to_owned()]);
}

#[test]
fn save_and_repack_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    // The builder reports an image larger than the bound: never saved.
    let mut r = request(dir.path());
    r.policy.max_archive_bytes = 1000;
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.reported_size = 1001;
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o1"))),
        "source_oci_artifact_bounds"
    );
    assert!(!b.calls.borrow().iter().any(|c| c == "save"));
    assert!(!dir.path().join("o1").exists());
    // The reported size passes but the repacked archive would not fit: the
    // writer stops at the bound and nothing is published.
    let b = fake(ok_config(), BuildOutcome::Built);
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o2"))),
        "source_oci_artifact_bounds"
    );
    assert!(!dir.path().join("o2").exists());
    assert_eq!(b.released.get(), 1);
}

#[test]
fn a_saved_archive_with_a_repeated_member_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (built, _, cd) = image_archive(ok_config());
    // Append a second member with the name of an existing blob.
    let mut members = Vec::new();
    let mut archive = tar::Archive::new(built.as_slice());
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        members.push((name, bytes));
    }
    members.push((blob_path(&cd), b"{\"os\":\"linux\"}".to_vec()));
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.built = tar_bytes(&members);
    let e = run(&request(dir.path()), &b, &dir.path().join("o"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, "source_oci_artifact_invalid");
    assert!(e.detail.contains("duplicate"), "{e}");
}

#[test]
fn artifact_checks_port_cmd_and_platform() {
    let dir = tempfile::tempdir().unwrap();
    let r = request(dir.path());
    for (n, (config, want)) in [
        (
            image_config(json!(["x"]), json!({"80/tcp":{}}), "amd64"),
            "source_oci_port_unmatched",
        ),
        (
            image_config(json!(["x"]), json!({"8080/tcp":{},"9000/tcp":{}}), "amd64"),
            "source_oci_port_unmatched",
        ),
        (
            image_config(json!(["x"]), json!({}), "amd64"),
            "source_oci_port_unmatched",
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
        let b = fake(config, BuildOutcome::Built);
        let out = dir.path().join(format!("o-{n}"));
        assert_eq!(code(run(&r, &b, &out)), want);
        assert!(b.store.borrow().is_empty(), "release after {want}");
        assert!(!out.exists());
    }
    // A saved archive whose config is not the built image is refused.
    let mut b = fake(ok_config(), BuildOutcome::Built);
    b.built_id = format!("sha256:{}", "3".repeat(64));
    assert_eq!(
        code(run(&r, &b, &dir.path().join("o9"))),
        "source_oci_artifact_digest_mismatch"
    );
}

#[test]
fn a_tag_reference_is_resolved_only_through_an_explicit_pinned_digest() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(dir.path());
    r.base_images[0].reference = "base:1".into();
    let b = fake(ok_config(), BuildOutcome::Built);
    let m = run(&r, &b, &dir.path().join("out")).unwrap();
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
        code(run(&r, &b, &dir.path().join("o"))),
        "source_oci_base_reference_invalid"
    );
}

/// `out` must be refused, untouched, before any load or build, with the
/// builder still released.
fn assert_refused_output(req: &SourceOciRequest, out: &Path) {
    let b = fake(ok_config(), BuildOutcome::Built);
    assert_eq!(
        code(run(req, &b, out)),
        "source_oci_output_invalid",
        "{out:?}"
    );
    assert!(
        !b.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("build") || c.starts_with("load")),
        "{out:?}"
    );
    assert_eq!(b.released.get(), 1, "{out:?}");
}

#[test]
fn an_existing_output_directory_or_file_is_refused_untouched_and_the_builder_is_still_released() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    // An existing directory holding someone else's data.
    let existing = dir.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("sentinel"), b"keep me").unwrap();
    // An existing file.
    let file = dir.path().join("file");
    std::fs::write(&file, b"keep this file").unwrap();
    assert_refused_output(&req, &existing);
    assert_refused_output(&req, &file);
    assert_eq!(
        std::fs::read(existing.join("sentinel")).unwrap(),
        b"keep me"
    );
    assert_eq!(std::fs::read_dir(&existing).unwrap().count(), 1);
    assert_eq!(std::fs::read(&file).unwrap(), b"keep this file");
}

#[cfg(unix)]
#[test]
fn an_existing_output_symlink_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    // A symlink to a directory with data, and a dangling symlink.
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("sentinel"), b"keep target").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("absent"), &dangling).unwrap();
    assert_refused_output(&req, &link);
    assert_refused_output(&req, &dangling);
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert_eq!(
        std::fs::read(target.join("sentinel")).unwrap(),
        b"keep target"
    );
    assert!(
        std::fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dir.path().join("absent").exists());
}
