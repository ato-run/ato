mod fail_closed_support;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use fail_closed_support::spawn_static_file_server;
#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tempfile::TempDir;
#[cfg(unix)]
use zip::{write::FileOptions, ZipWriter};

#[cfg(unix)]
struct MockGitHubArchiveServer {
    base_url: String,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for MockGitHubArchiveServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock GitHub archive server thread");
        }
    }
}

#[cfg(unix)]
struct FakeOllamaServer {
    base_url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for FakeOllamaServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(
            self.base_url
                .trim_start_matches("http://")
                .trim_start_matches("https://"),
        );
        if let Some(handle) = self.handle.take() {
            handle.join().expect("fake Ollama server thread");
        }
    }
}

#[cfg(unix)]
struct TestWheelSpec<'a> {
    package_name: &'a str,
    version: &'a str,
    module_name: &'a str,
    module_files: Vec<(&'a str, &'a str)>,
}

#[cfg(unix)]
fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    fs::create_dir_all(&root).expect("create workspace .tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create workspace tempdir")
}

#[cfg(unix)]
fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(unix)]
fn host_limited(stderr: &str) -> bool {
    stderr.contains("No compatible native sandbox backend is available")
        || stderr.contains("Sandbox unavailable")
        || stderr.contains("pfctl failed to load anchor")
}

#[cfg(unix)]
fn maybe_resolve_test_nacelle_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("NACELLE_PATH") {
        let nacelle = PathBuf::from(path);
        if nacelle.exists() {
            return Some(nacelle);
        }
    }

    let candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../nacelle/target/debug/nacelle");
    candidate.exists().then_some(candidate)
}

#[cfg(unix)]
fn maybe_resolve_uv_path() -> Option<PathBuf> {
    which::which("uv").ok()
}

#[cfg(unix)]
fn require_native_prerequisites() -> Option<PathBuf> {
    let Some(nacelle) = maybe_resolve_test_nacelle_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires nacelle for GitHub source sandbox E2E"
        );
        return None;
    };
    let Some(_uv) = maybe_resolve_uv_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires uv for GitHub source sandbox E2E"
        );
        return None;
    };
    Some(nacelle)
}

#[cfg(unix)]
fn assert_success_or_skip(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && host_limited(&stderr) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={stderr}"
        );
        return false;
    }

    assert!(
        output.status.success(),
        "stdout={}; stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    true
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
fn spawn_github_archive_server(
    _expected_path: &'static str,
    archive: Vec<u8>,
) -> MockGitHubArchiveServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let addr = listener.local_addr().expect("listener addr");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).expect("read request");
        let (status_line, response_body, content_type) =
            ("HTTP/1.1 200 OK", archive, "application/gzip");
        let response = format!(
            "{status_line}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .and_then(|_| stream.write_all(&response_body))
            .expect("write response");
    });

    MockGitHubArchiveServer {
        base_url: format!("http://{}", addr),
        handle: Some(handle),
    }
}

#[cfg(unix)]
fn spawn_fake_ollama_server(models: &[&str], completion_text: &str) -> FakeOllamaServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake Ollama");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking fake Ollama");
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_flag = Arc::clone(&shutdown);
    let models_json = serde_json::json!({
        "models": models.iter().map(|name| serde_json::json!({ "name": name })).collect::<Vec<_>>()
    })
    .to_string();
    let completion_json = serde_json::json!({
        "choices": [{ "message": { "content": completion_text } }]
    })
    .to_string();

    let handle = thread::spawn(move || {
        while !shutdown_flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0u8; 8192];
                    let size = match stream.read(&mut request) {
                        Ok(size) => size,
                        Err(_) => continue,
                    };
                    let request_text = String::from_utf8_lossy(&request[..size]);
                    let first_line = request_text.lines().next().unwrap_or_default();
                    let (status_line, response_body) = if first_line.starts_with("GET /api/tags ") {
                        ("HTTP/1.1 200 OK", models_json.clone())
                    } else if first_line.starts_with("POST /v1/chat/completions ") {
                        ("HTTP/1.1 200 OK", completion_json.clone())
                    } else {
                        (
                            "HTTP/1.1 404 Not Found",
                            "{\"error\":\"not found\"}".to_string(),
                        )
                    };
                    let response = format!(
                        "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    FakeOllamaServer {
        base_url: format!("http://{}", addr),
        shutdown,
        handle: Some(handle),
    }
}

#[cfg(unix)]
fn write_test_wheel(root: &Path, spec: TestWheelSpec<'_>) -> String {
    let normalized = spec.package_name.replace('-', "_");
    let wheel_name = format!("{normalized}-{}-py3-none-any.whl", spec.version);
    let wheel_path = root.join("packages").join(&wheel_name);
    fs::create_dir_all(wheel_path.parent().expect("wheel parent")).expect("create packages dir");

    let file = File::create(&wheel_path).expect("create wheel");
    let mut zip = ZipWriter::new(file);
    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let metadata = format!(
        "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
        spec.package_name, spec.version
    );
    let wheel = "\
Wheel-Version: 1.0\n\
Generator: ato-cli-test\n\
Root-Is-Purelib: true\n\
Tag: py3-none-any\n";
    let mut record_lines = vec![
        format!("{normalized}-{}.dist-info/METADATA,,", spec.version),
        format!("{normalized}-{}.dist-info/WHEEL,,", spec.version),
        format!("{normalized}-{}.dist-info/RECORD,,", spec.version),
    ];

    for (relative_path, contents) in spec.module_files {
        record_lines.push(format!("{}/{relative_path},,", spec.module_name));
        zip.start_file(format!("{}/{}", spec.module_name, relative_path), options)
            .expect("start module file");
        zip.write_all(contents.as_bytes())
            .expect("write module file");
    }

    zip.start_file(
        format!("{normalized}-{}.dist-info/METADATA", spec.version),
        options,
    )
    .expect("start METADATA");
    zip.write_all(metadata.as_bytes()).expect("write METADATA");

    zip.start_file(
        format!("{normalized}-{}.dist-info/WHEEL", spec.version),
        options,
    )
    .expect("start WHEEL");
    zip.write_all(wheel.as_bytes()).expect("write WHEEL");

    zip.start_file(
        format!("{normalized}-{}.dist-info/RECORD", spec.version),
        options,
    )
    .expect("start RECORD");
    zip.write_all((record_lines.join("\n") + "\n").as_bytes())
        .expect("write RECORD");

    zip.finish().expect("finish wheel");
    wheel_name
}

#[cfg(unix)]
fn write_simple_index(root: &Path, packages: &[(&str, Vec<String>)]) {
    let simple_dir = root.join("simple");
    fs::create_dir_all(&simple_dir).expect("create simple root index");
    let mut root_index = String::new();
    for (package_name, wheel_names) in packages {
        let package_dir = simple_dir.join(package_name);
        fs::create_dir_all(&package_dir).expect("create package index dir");
        root_index.push_str(&format!(
            "<!doctype html><a href=\"{package_name}/\">{package_name}</a>\n"
        ));
        let package_index = wheel_names
            .iter()
            .map(|wheel_name| {
                format!("<!doctype html><a href=\"../../packages/{wheel_name}\">{wheel_name}</a>\n")
            })
            .collect::<String>();
        fs::write(package_dir.join("index.html"), package_index)
            .expect("write package simple index");
    }
    fs::write(simple_dir.join("index.html"), root_index).expect("write simple root index");
}

#[cfg(unix)]
fn fixture_helper_source() -> &'static str {
    r#"HELPER_LOADED = True


def render_output(text: str) -> str:
    return f"helper::{text}\n"
"#
}

#[cfg(unix)]
fn fixture_translator_source() -> &'static str {
    r#"from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from urllib import request

import demo_translate_helper

API_BASE = os.environ.get("OLLAMA_API_BASE", "http://127.0.0.1:11434")
API_MODEL = "qwen2:7b"


def translate_markdown(content: str, base_lang: str, target_lang: str) -> str:
    payload = {
        "model": API_MODEL,
        "messages": [
            {"role": "system", "content": f"Translate markdown from {base_lang} to {target_lang}."},
            {"role": "user", "content": content},
        ],
    }
    req = request.Request(
        f"{API_BASE}/v1/chat/completions",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with request.urlopen(req) as resp:
        body = json.loads(resp.read().decode("utf-8"))
    translated = body["choices"][0]["message"]["content"]
    return demo_translate_helper.render_output(translated)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-lang", required=True)
    parser.add_argument("--target-lang", required=True)
    parser.add_argument("--input-dir", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--recursive", action="store_true")
    args = parser.parse_args()

    input_dir = Path(args.input_dir)
    output_dir = Path(args.output_dir)
    files = sorted(input_dir.rglob("*.md") if args.recursive else input_dir.glob("*.md"))
    output_dir.mkdir(parents=True, exist_ok=True)

    for source_path in files:
        relative_path = source_path.relative_to(input_dir)
        destination = output_dir / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(
            translate_markdown(
                source_path.read_text(encoding="utf-8"),
                args.base_lang,
                args.target_lang,
            ),
            encoding="utf-8",
        )

    probe_path = os.environ.get("OLLAMA_TRANSLATOR_PROBE_PATH")
    if probe_path:
        Path(probe_path).write_text(
            json.dumps(
                {
                    "cwd": os.getcwd(),
                    "argv": list(__import__("sys").argv[1:]),
                    "api_base": API_BASE,
                    "model": API_MODEL,
                    "helper_loaded": bool(getattr(demo_translate_helper, "HELPER_LOADED", False)),
                },
                ensure_ascii=True,
            ),
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#
}

#[cfg(unix)]
fn fixture_capsule_manifest(ollama_base_url: &str) -> String {
    format!(
        r#"schema_version = "0.2"
name = "ollama-translator"
version = "0.1.0"
type = "job"
default_target = "translate"

[targets.translate]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "ollama_translator.py"
source_layout = "anchored_entrypoint"

[services.ollama]
from = "dependency:ollama"
mode = "reuse-if-present"
lifecycle = "managed"

[services.ollama.healthcheck]
kind = "http"
url = "{ollama_base_url}/api/tags"

[bootstrap.defaults]
ollama_model = "qwen2:7b"
"#
    )
}

#[cfg(unix)]
fn load_json(path: &Path) -> Value {
    let raw = fs::read(path).expect("read json file");
    serde_json::from_slice(&raw).expect("parse json")
}

#[cfg(unix)]
fn provider_runs_under(caller_dir: &Path) -> PathBuf {
    caller_dir.join(".ato").join("tmp").join("gh-install")
}

#[cfg(unix)]
#[test]
#[serial]
fn github_source_python_run_uses_repo_manifest_and_ollama_preflight() {
    let Some(nacelle) = require_native_prerequisites() else {
        return;
    };

    let temp = workspace_tempdir("github-ollama-run-");
    let caller_dir = temp.path().join("caller");
    let index_root = temp.path().join("index");
    let home = workspace_tempdir("github-ollama-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&index_root).expect("create index dir");

    let helper_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "demo-translate-helper",
            version: "0.1.0",
            module_name: "demo_translate_helper",
            module_files: vec![("__init__.py", fixture_helper_source())],
        },
    );
    write_simple_index(
        &index_root,
        &[("demo-translate-helper", vec![helper_wheel])],
    );
    let index_server = spawn_static_file_server(index_root.clone());
    let ollama_server = spawn_fake_ollama_server(&["qwen2:7b"], "translated-by-fake-ollama");

    let archive = build_github_tarball(
        "wolfreka-ollama-translator-abcdef",
        &[
            (
                "capsule.toml",
                &fixture_capsule_manifest(ollama_server.base_url.as_str()),
            ),
            ("requirements.txt", "demo-translate-helper==0.1.0\n"),
            ("uv.lock", "version = 1\n"),
            ("ollama_translator.py", fixture_translator_source()),
        ],
    );
    let github_server =
        spawn_github_archive_server("/repos/wolfreka/ollama-translator/tarball", archive);

    let input_dir = caller_dir.join("md-input");
    let output_dir = caller_dir.join("md-ja");
    let probe_path = caller_dir.join("probe.json");
    fs::create_dir_all(&input_dir).expect("create input dir");
    fs::create_dir_all(&output_dir).expect("create output dir");
    fs::write(input_dir.join("hello.md"), "# Hello\n\nsource markdown\n").expect("write input");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--sandbox")
        .arg("--yes")
        .arg("--nacelle")
        .arg(&nacelle)
        .arg("--read")
        .arg("./md-input")
        .arg("--read-write")
        .arg("./md-ja")
        .arg("github.com/wolfreka/ollama-translator")
        .arg("--")
        .args([
            "--base-lang",
            "en",
            "--target-lang",
            "ja",
            "--input-dir",
            "./md-input",
            "--output-dir",
            "./md-ja",
            "--recursive",
        ])
        .env("HOME", home.path())
        .env("ATO_TOKEN", "test-token")
        .env("ATO_GITHUB_API_BASE_URL", github_server.base_url.as_str())
        .env(
            "UV_INDEX_URL",
            format!("{}/simple", index_server.base_url.as_str()),
        )
        .env(
            "PIP_INDEX_URL",
            format!("{}/simple", index_server.base_url.as_str()),
        )
        .env("UV_INSECURE_HOST", "127.0.0.1")
        .env("OLLAMA_API_BASE", ollama_server.base_url.as_str())
        .env("OLLAMA_TRANSLATOR_PROBE_PATH", &probe_path)
        .output()
        .expect("run GitHub source ollama fixture");

    if !assert_success_or_skip(&output) {
        return;
    }

    let translated = fs::read_to_string(output_dir.join("hello.md")).expect("read translated md");
    assert_eq!(translated, "helper::translated-by-fake-ollama\n");

    let probe = load_json(&probe_path);
    assert_eq!(
        probe["cwd"].as_str(),
        Some(
            caller_dir
                .canonicalize()
                .expect("canonical caller dir")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        probe["argv"],
        serde_json::json!([
            "--base-lang",
            "en",
            "--target-lang",
            "ja",
            "--input-dir",
            "./md-input",
            "--output-dir",
            "./md-ja",
            "--recursive"
        ])
    );
    assert_eq!(probe["model"].as_str(), Some("qwen2:7b"));
    assert_eq!(
        probe["api_base"].as_str(),
        Some(ollama_server.base_url.as_str())
    );
    assert_eq!(probe["helper_loaded"].as_bool(), Some(true));

    let checkout_root = provider_runs_under(&caller_dir);
    let has_remaining_checkout = if checkout_root.is_dir() {
        fs::read_dir(&checkout_root)
            .expect("read checkout root")
            .next()
            .is_some()
    } else {
        false
    };
    assert!(
        !has_remaining_checkout,
        "transient GitHub checkout should be cleaned up under {}",
        checkout_root.display()
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn github_source_python_run_errors_when_ollama_is_unreachable() {
    let temp = workspace_tempdir("github-ollama-missing-service-");
    let caller_dir = temp.path().join("caller");
    fs::create_dir_all(&caller_dir).expect("create caller dir");

    let archive = build_github_tarball(
        "wolfreka-ollama-translator-abcdef",
        &[
            (
                "capsule.toml",
                &fixture_capsule_manifest("http://127.0.0.1:9"),
            ),
            ("requirements.txt", ""),
            ("ollama_translator.py", fixture_translator_source()),
        ],
    );
    let github_server =
        spawn_github_archive_server("/repos/wolfreka/ollama-translator/tarball", archive);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("github.com/wolfreka/ollama-translator")
        .arg("--")
        .args([
            "--base-lang",
            "en",
            "--target-lang",
            "ja",
            "--input-dir",
            "./md-input",
            "--output-dir",
            "./md-ja",
        ])
        .env("ATO_TOKEN", "test-token")
        .env("ATO_GITHUB_API_BASE_URL", github_server.base_url.as_str())
        .output()
        .expect("run GitHub source missing Ollama fixture");

    assert!(!output.status.success(), "missing Ollama should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Ollama is not reachable at http://127.0.0.1:9/api/tags"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("Start or install Ollama, then retry"),
        "stderr={stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn github_source_python_run_errors_when_required_ollama_model_is_missing() {
    let temp = workspace_tempdir("github-ollama-missing-model-");
    let caller_dir = temp.path().join("caller");
    fs::create_dir_all(&caller_dir).expect("create caller dir");

    let ollama_server = spawn_fake_ollama_server(&["llama3:8b"], "unused");
    let archive = build_github_tarball(
        "wolfreka-ollama-translator-abcdef",
        &[
            (
                "capsule.toml",
                &fixture_capsule_manifest(ollama_server.base_url.as_str()),
            ),
            ("requirements.txt", ""),
            ("ollama_translator.py", fixture_translator_source()),
        ],
    );
    let github_server =
        spawn_github_archive_server("/repos/wolfreka/ollama-translator/tarball", archive);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--yes")
        .arg("github.com/wolfreka/ollama-translator")
        .arg("--")
        .args([
            "--base-lang",
            "en",
            "--target-lang",
            "ja",
            "--input-dir",
            "./md-input",
            "--output-dir",
            "./md-ja",
        ])
        .env("ATO_TOKEN", "test-token")
        .env("ATO_GITHUB_API_BASE_URL", github_server.base_url.as_str())
        .output()
        .expect("run GitHub source missing model fixture");

    assert!(!output.status.success(), "missing Ollama model should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Required Ollama model \"qwen2:7b\" is missing"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("Run: ollama pull qwen2:7b"),
        "stderr={stderr}"
    );
}
