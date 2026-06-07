//! Clean-VM regression guard for the gitless GitHub source install path.
//!
//! `ato install`/`ato run github.com/<repo>` must NOT require the `git` CLI: the
//! source is fetched via the GitHub tarball API (not `git clone`), and manifest
//! inference + preview never spawn `git`. A user empirically confirmed on a clean
//! VM (no git) that the install used to fail; this test reproduces a no-git host
//! hermetically and asserts the fetch + inference path is gitless.
//!
//! The assertion is deliberately scoped to the *gitless property*: with `git`
//! removed from PATH, the run must reach the dependency/provision stage (proving
//! fetch + inference succeeded without git) and must never fail with a
//! git-not-found error attributable to Ato's own source fetch. It does not assert
//! a full successful sandboxed run, because that additionally needs a reachable
//! PyPI seed index and a native sandbox backend — both orthogonal to git.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::thread;

#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
struct MockServer {
    base_url: String,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server thread");
        }
    }
}

#[cfg(unix)]
fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create workspace .ato/test-scratch");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create workspace tempdir")
}

#[cfg(unix)]
fn build_github_tarball(root: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("{root}/{path}"),
                std::io::Cursor::new(contents.as_bytes()),
            )
            .expect("append tar entry");
    }
    builder
        .into_inner()
        .expect("finish tar builder")
        .finish()
        .expect("finish gzip encoder");
    bytes
}

#[cfg(unix)]
fn spawn_github_archive_server(archive: Vec<u8>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind github server");
    let addr = listener.local_addr().expect("github listener addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept github request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).expect("read github request");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            archive.len()
        );
        stream
            .write_all(response.as_bytes())
            .and_then(|_| stream.write_all(&archive))
            .expect("write github response");
    });
    MockServer {
        base_url: format!("http://{}", addr),
        handle: Some(handle),
    }
}

#[cfg(unix)]
fn spawn_store_install_draft_server(
    owner: &'static str,
    repo: &'static str,
    resolved_sha: &'static str,
) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind store server");
    let addr = listener.local_addr().expect("store listener addr");
    let response_body = serde_json::json!({
        "repo": {
            "owner": owner,
            "repo": repo,
            "fullName": format!("{owner}/{repo}"),
            "defaultBranch": "main"
        },
        "capsuleToml": { "exists": true },
        "repoRef": format!("{owner}/{repo}"),
        "proposedRunCommand": null,
        "proposedInstallCommand": "",
        "resolvedRef": { "ref": "refs/heads/main", "sha": resolved_sha },
        "manifestSource": "repository",
        "previewToml": null,
        "capsuleHint": null,
        "inferenceMode": "manifest",
        "retryable": false
    })
    .to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept store request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).expect("read store request");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write store response");
    });
    MockServer {
        base_url: format!("http://{}", addr),
        handle: Some(handle),
    }
}

/// Build a temp bin directory that symlinks every tool the run path may legitimately
/// invoke (sh, env, uv, deno, python3, …) EXCEPT `git`, plus a poison `git` stub that
/// fails loudly. The returned PATH contains ONLY this directory, so any `git` lookup
/// resolves to the poison stub (or fails), modelling a clean VM without git.
#[cfg(unix)]
fn scrubbed_no_git_path(scratch: &Path) -> (PathBuf, String) {
    let bin = scratch.join("nogit-bin");
    fs::create_dir_all(&bin).expect("create nogit bin dir");

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let path_dirs: Vec<PathBuf> = std::env::split_paths(&existing_path).collect();
    let tools = [
        "sh", "bash", "env", "uv", "deno", "python3", "python", "node", "npm", "sed", "awk",
        "grep", "cat", "ls", "cp", "mv", "rm", "mkdir", "chmod", "ln", "tar", "gzip", "curl",
        "dirname", "basename", "uname", "true", "false", "test", "head", "tail", "tr", "cut",
        "sort", "find", "xargs", "touch", "id", "whoami", "stat", "readlink", "file", "which",
    ];
    for tool in tools {
        if tool == "git" {
            continue;
        }
        for dir in &path_dirs {
            let candidate = dir.join(tool);
            if candidate.exists() {
                let _ = symlink(&candidate, bin.join(tool));
                break;
            }
        }
    }

    // Poison `git` so any accidental invocation is unambiguous in failure output —
    // it must never be reached by the Ato source-fetch / inference path.
    let git_stub = bin.join("git");
    fs::write(
        &git_stub,
        "#!/bin/sh\necho 'gitless-regression: git must not be invoked on the install path' >&2\nexit 127\n",
    )
    .expect("write poison git stub");
    let mut perms = fs::metadata(&git_stub)
        .expect("stat git stub")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&git_stub, perms).expect("chmod git stub");

    let path_value = bin.to_string_lossy().to_string();
    (bin, path_value)
}

#[cfg(unix)]
fn capsule_manifest() -> &'static str {
    r#"schema_version = "0.3"
name = "gitless-probe"
version = "0.1.0"
type = "job"

runtime = "source/python"
runtime_version = "3.11.10"
source_layout = "anchored_entrypoint"
run = "main.py"
"#
}

#[cfg(unix)]
fn noop_source() -> &'static str {
    "def main() -> int:\n    return 0\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n"
}

/// Regression: with `git` absent from PATH, `ato run github.com/<repo>` must fetch
/// the source tarball and infer its manifest WITHOUT spawning git. The poison git
/// stub guarantees that any git invocation on the install path would surface a
/// distinctive marker; the run must never hit it.
#[cfg(unix)]
#[test]
#[serial]
fn github_source_run_fetches_and_infers_without_git() {
    let scratch = workspace_tempdir("gitless-install-");
    let caller_dir = scratch.path().join("caller");
    let home = scratch.path().join("home");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&home).expect("create home dir");

    let archive = build_github_tarball(
        "wolfreka-gitless-probe-abcdef",
        &[
            ("capsule.toml", capsule_manifest()),
            ("requirements.txt", ""),
            ("main.py", noop_source()),
        ],
    );
    let github_server = spawn_github_archive_server(archive);
    let store_server = spawn_store_install_draft_server("wolfreka", "gitless-probe", "abcdef");

    let (_bin, no_git_path) = scrubbed_no_git_path(scratch.path());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("github.com/wolfreka/gitless-probe")
        // env_clear so the inherited (git-bearing) PATH cannot leak in; we hand the
        // child ONLY the scrubbed PATH, modelling a clean VM without git.
        .env_clear()
        .env("PATH", &no_git_path)
        .env("HOME", &home)
        .env("ATO_TOKEN", "test-token")
        .env("ATO_STORE_API_URL", store_server.base_url.as_str())
        .env("ATO_GITHUB_API_BASE_URL", github_server.base_url.as_str())
        .output()
        .expect("run gitless GitHub source fixture");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // 1) The poison git stub must never have been reached on the install path.
    assert!(
        !combined.contains("gitless-regression: git must not be invoked"),
        "Ato spawned git on the source install path; this breaks gitless install.\n\
         stdout={stdout}\nstderr={stderr}"
    );

    // 2) No git-not-found error attributable to Ato's own fetch.
    assert!(
        !combined.contains("git: command not found")
            && !combined.contains("CommandNotFound")
            && !combined.contains("Failed to execute git"),
        "Ato's source fetch reported git missing; fetch must be gitless.\n\
         stdout={stdout}\nstderr={stderr}"
    );

    // 3) The gitless fetch + manifest inference must have succeeded: the run must
    //    have progressed to dependency/provision/execution. We accept either a
    //    fully successful run or a downstream failure (no PyPI seed / no native
    //    sandbox), but the presence of provision/run progress proves fetch +
    //    inference happened gitlessly. The one thing we reject is failing BEFORE
    //    reaching that stage with a git-shaped error (already asserted above).
    let reached_provision = combined.contains("isolated run workspace")
        || combined.contains("Provision")
        || combined.contains("Dependency cache")
        || combined.contains("dependency materialization")
        || output.status.success();
    assert!(
        reached_provision,
        "gitless run did not reach provision/inference stage; fetch may have failed.\n\
         status={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status
    );
}
