//! Track C PR 2a (#912): build a bootable ext4 rootfs from an approved capsule source.
//!
//! Given a materialized source dir (or a server-resolved owner/repo/commit) containing a
//! `capsule.toml`, this parses the manifest, **fails closed** on any unsupported shape
//! (secrets/bindings/external/GPU/unknown-runtime/missing port|healthcheck), and drives
//! the Docker→ext4 assembly. Docker is a build tool, not the trust boundary. Prints a
//! non-secret JSON receipt. dist=false (dev/builder tool, not shipped).
//!
//! ```sh
//! rootfs_build --source ./capsule --out app.ext4 --size-mib 1024
//! rootfs_build --owner acme --repo app --commit <40-hex> --out app.ext4
//! ```

use std::path::{Path, PathBuf};

use capsule::foundation::types::manifest::CapsuleManifest;
use snapshot::rootfs_builder::{
    SourceProbe, build_rootfs, derive_build_spec, derive_supervisor_build_spec, materialize_source,
    valid_github_owner, valid_github_repo,
};

fn arg(flags: &[&str]) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    for f in flags {
        if let Some(i) = a.iter().position(|x| x == f)
            && i + 1 < a.len()
        {
            return Some(a[i + 1].clone());
        }
    }
    None
}

fn fail(stage: &str, reason: String) -> ! {
    eprintln!(
        "{}",
        serde_json::json!({ "ok": false, "failure_stage": stage, "failure_reason": reason })
    );
    std::process::exit(1);
}

fn main() {
    let out = PathBuf::from(
        arg(&["--out", "-o"]).unwrap_or_else(|| fail("args", "--out <ext4> required".into())),
    );
    let size_mib: u64 = arg(&["--size-mib"])
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);

    // Source: a local checkout, or materialize a server-resolved owner/repo/commit.
    let src: PathBuf = if let Some(dir) = arg(&["--source", "-s"]) {
        PathBuf::from(dir)
    } else {
        let owner = arg(&["--owner"]).unwrap_or_else(|| {
            fail(
                "args",
                "--source or --owner/--repo/--commit required".into(),
            )
        });
        let repo = arg(&["--repo"]).unwrap_or_else(|| fail("args", "--repo required".into()));
        let commit =
            arg(&["--commit"]).unwrap_or_else(|| fail("args", "--commit (40-hex) required".into()));
        let subdir = arg(&["--subdir"]);
        // Validate identity BEFORE it is used to build a filesystem path (belt + suspenders;
        // materialize_source re-validates). A path-like repo must never shape the work dir.
        if !valid_github_owner(&owner) {
            fail("source", format!("invalid github owner {owner:?}"));
        }
        if !valid_github_repo(&repo) {
            fail("source", format!("invalid github repo {repo:?}"));
        }
        let work = std::env::temp_dir().join(format!("rootfs-build-{repo}"));
        let _ = std::fs::remove_dir_all(&work);
        // Dev tool: no recipe manifest override (#932) — the checkout must carry its own
        // capsule.toml, matching the raw-GitHub builder path.
        materialize_source(&owner, &repo, &commit, subdir.as_deref(), None, &work)
            .unwrap_or_else(|e| fail("source", e))
    };

    let toml_path = src.join("capsule.toml");
    let toml = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| fail("source", format!("read {}: {e}", toml_path.display())));
    let manifest = CapsuleManifest::from_toml(&toml)
        .unwrap_or_else(|e| fail("manifest", format!("parse capsule.toml: {e}")));

    let probe = SourceProbe::scan(&src);
    // v1.2: a capsule that declares [secrets.*] is a SUPERVISOR build (env-delivery) —
    // the rootfs runs the guest-agent as init + carries /etc/ato/supervisor.json (needs
    // ATO_GUEST_AGENT_BIN). A no-secret capsule uses the v1.0 no-binding path.
    let spec = if manifest.secrets.is_empty() {
        derive_build_spec(&manifest, &probe).unwrap_or_else(|e| fail("eligibility", e))
    } else {
        derive_supervisor_build_spec(&manifest, &probe).unwrap_or_else(|e| fail("eligibility", e))
    };

    let receipt = build_rootfs(Path::new(&src), &spec, &out, size_mib)
        .unwrap_or_else(|e| fail("rootfs_build", e));
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "ok": true, "receipt": receipt }))
            .unwrap()
    );
}
