//! v1.7 Dockerfile-to-Snapshot Import (ato#994 PR 5): dev/builder CLI — drive one
//! Dockerfile import end to end on a builder host and print the non-secret JSON
//! receipt. Mirrors `rootfs_build.rs` (dist=false, operator tool, fail-closed).
//!
//! The output ext4 is a normal Ato supervisor rootfs: feed it to the existing
//! Ready-State build (boot → verify healthcheck → snapshot → seal) unchanged —
//! nothing Docker-shaped survives into the artifact.
//!
//! ```sh
//! docker_import_build --source ./checkout --out app.ext4 \
//!   [--dockerfile Dockerfile] [--size-mib 2048] [--port N] \
//!   [--readiness-path /health] [--convert-placeholder-secrets] \
//!   [--build-arg K=V]... [--tag ato-import-<name>]
//! ```
//!
//! Host requirements: podman (preferred) or docker, root (mkfs/mount), and
//! `ATO_GUEST_AGENT_BIN` pointing at the guest-agent binary (imports always run
//! the supervisor; an empty binding set is vacuously bound-ready).

use std::collections::BTreeMap;
use std::path::PathBuf;

use snapshot::docker_import::build::SystemImportCommandRunner;
use snapshot::docker_import::{
    DockerImportSpec, DockerfileImportRequest, SecretEnvPolicy, import_identity_digest,
    run_dockerfile_import,
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

fn args_multi(flag: &str) -> Vec<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .enumerate()
        .filter(|(_, x)| x.as_str() == flag)
        .filter_map(|(i, _)| a.get(i + 1).cloned())
        .collect()
}

fn has_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

fn fail(stage: &str, reason: String) -> ! {
    eprintln!("{}", serde_json::json!({ "ok": false, "failure_stage": stage, "failure_reason": reason }));
    std::process::exit(1);
}

fn main() {
    let source = PathBuf::from(
        arg(&["--source", "-s"]).unwrap_or_else(|| fail("args", "--source <checkout dir> required".into())),
    );
    let out = PathBuf::from(arg(&["--out", "-o"]).unwrap_or_else(|| fail("args", "--out <ext4> required".into())));
    let dockerfile = arg(&["--dockerfile"]).unwrap_or_else(|| "Dockerfile".to_string());
    let size_mib: u64 = arg(&["--size-mib"]).and_then(|s| s.parse().ok()).unwrap_or(2048);
    let port_override: Option<u16> = arg(&["--port"])
        .map(|p| p.parse().unwrap_or_else(|_| fail("args", format!("--port {p:?} is not a u16"))));
    let readiness_http_path = arg(&["--readiness-path"]);
    let policy = if has_flag("--convert-placeholder-secrets") {
        SecretEnvPolicy::ConvertPlaceholders
    } else {
        SecretEnvPolicy::Reject
    };
    let image_tag = arg(&["--tag"]).unwrap_or_else(|| format!("ato-import-{}", std::process::id()));

    let mut build_args: BTreeMap<String, String> = BTreeMap::new();
    for kv in args_multi("--build-arg") {
        let Some((k, v)) = kv.split_once('=') else {
            fail("args", format!("--build-arg {kv:?} is not K=V"));
        };
        build_args.insert(k.to_string(), v.to_string());
    }

    let spec = DockerImportSpec::new(&dockerfile, build_args).unwrap_or_else(|e| fail("spec", e));
    let req = DockerfileImportRequest {
        context_dir: &source,
        spec,
        policy,
        port_override,
        readiness_http_path,
        image_tag,
        out_ext4: &out,
        size_mib,
    };

    let outcome =
        run_dockerfile_import(&SystemImportCommandRunner, &req).unwrap_or_else(|e| fail("import", e));

    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "receipt": outcome.receipt,
            "import_identity": import_identity_digest(&outcome.receipt),
            "rootfs_path": outcome.rootfs_path,
            "rootfs_bytes": outcome.rootfs_bytes,
            "plan": {
                "port": outcome.plan.port,
                "binding_names": outcome.plan.supervisor.binding_names,
                "public_service": outcome.plan.supervisor.public_service,
                "warnings": outcome.plan.warnings,
            },
        })
    );
}
