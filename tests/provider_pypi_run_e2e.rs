mod fail_closed_support;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};

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
struct TestWheelSpec<'a> {
    package_name: &'a str,
    version: &'a str,
    module_name: &'a str,
    module_files: Vec<(&'a str, &'a str)>,
    metadata_lines: Vec<String>,
    console_script_name: Option<&'a str>,
    console_script_entrypoint: Option<&'a str>,
}

#[cfg(unix)]
fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".tmp");
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
fn require_native_provider_prerequisites() -> Option<PathBuf> {
    let Some(nacelle) = maybe_resolve_test_nacelle_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires nacelle for provider-backed sandbox E2E"
        );
        return None;
    };
    let Some(_uv) = maybe_resolve_uv_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires uv for provider-backed sandbox E2E"
        );
        return None;
    };
    Some(nacelle)
}

#[cfg(unix)]
fn require_provider_materialization_prerequisites() -> bool {
    if maybe_resolve_uv_path().is_some() {
        return true;
    }

    assert!(
        !strict_ci(),
        "strict CI requires uv for provider-backed materialization E2E"
    );
    false
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
fn load_output_json(path: &Path, output: &std::process::Output) -> Value {
    for _ in 0..100 {
        if let Ok(raw) = fs::read(path) {
            return serde_json::from_slice(&raw).expect("parse output json");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let raw = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "read output json: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    serde_json::from_slice(&raw).expect("parse output json")
}

#[cfg(unix)]
fn write_poison_python_shims(root: &Path) {
    let script = "#!/bin/sh\necho poisoned python shim >&2\nexit 97\n";
    for name in ["python", "python3"] {
        let path = root.join(name);
        fs::write(&path, script).expect("write poison python shim");
        let mut permissions = fs::metadata(&path).expect("shim metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("shim permissions");
    }
}

#[cfg(unix)]
fn prepend_path(dir: &Path) -> String {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths)
        .expect("join PATH entries")
        .to_string_lossy()
        .to_string()
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

    let mut metadata = format!(
        "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
        spec.package_name, spec.version
    );
    for line in &spec.metadata_lines {
        metadata.push_str(line);
        metadata.push('\n');
    }
    let wheel = "\
Wheel-Version: 1.0\n\
Generator: ato-cli-test\n\
Root-Is-Purelib: true\n\
Tag: py3-none-any\n";
    let entry_points = spec
        .console_script_name
        .zip(spec.console_script_entrypoint)
        .map(|(name, entrypoint)| format!("[console_scripts]\n{name} = {entrypoint}\n"));
    let has_explicit_init = spec
        .module_files
        .iter()
        .any(|(relative_path, _)| *relative_path == "__init__.py");
    let mut record_lines = vec![
        format!("{normalized}-{}.dist-info/METADATA,,", spec.version),
        format!("{normalized}-{}.dist-info/WHEEL,,", spec.version),
        format!("{normalized}-{}.dist-info/RECORD,,", spec.version),
    ];

    if !has_explicit_init {
        record_lines.push(format!("{}/__init__.py,,", spec.module_name));
        zip.start_file(format!("{}/__init__.py", spec.module_name), options)
            .expect("start __init__.py");
        zip.write_all(b"").expect("write __init__.py");
    }

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

    if let Some(entry_points) = entry_points {
        record_lines.push(format!(
            "{normalized}-{}.dist-info/entry_points.txt,,",
            spec.version
        ));
        zip.start_file(
            format!("{normalized}-{}.dist-info/entry_points.txt", spec.version),
            options,
        )
        .expect("start entry_points.txt");
        zip.write_all(entry_points.as_bytes())
            .expect("write entry_points.txt");
    }

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
fn provider_runs_root(home: &Path) -> PathBuf {
    home.join(".ato").join("runs").join("provider-backed")
}

#[cfg(unix)]
fn demo_provider_cli_source() -> &'static str {
    r#"from __future__ import annotations

import argparse
import json
import os
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_path")
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args()

    helper_available = False
    try:
        import demo_provider_pdf_helper

        helper_available = bool(getattr(demo_provider_pdf_helper, "PDF_HELPER", False))
    except ImportError:
        helper_available = False

    input_path = Path(args.input_path)
    output_path = Path(args.output)
    payload = {
        "cwd": os.getcwd(),
        "argv": [args.input_path, "-o", args.output],
        "input_exists": input_path.exists(),
        "content": input_path.read_text(encoding="utf-8"),
        "helper_available": helper_available,
        "python_version": list(__import__("sys").version_info[:3]),
        "python_executable": __import__("sys").executable,
    }
    output_path.write_text(json.dumps(payload, ensure_ascii=True), encoding="utf-8")
    print(json.dumps(payload, ensure_ascii=True))
    return 0
"#
}

#[cfg(unix)]
fn fake_markitdown_cli_source() -> &'static str {
    r#"from __future__ import annotations

import argparse
from pathlib import Path


class MissingDependencyException(RuntimeError):
    pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_path")
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args()

    input_path = Path(args.input_path)
    output_path = Path(args.output)

    if input_path.suffix.lower() == ".pdf":
        try:
            import markitdown_pdf_helper  # noqa: F401
        except ImportError as exc:
            raise MissingDependencyException(
                "PdfConverter recognized the input as a potential .pdf file, but the dependencies needed to read .pdf files have not been installed. To resolve this error, include the optional dependency [pdf] or [all] when installing MarkItDown."
            ) from exc

    output_path.write_text('converted\n', encoding="utf-8")
    return 0
"#
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_pypi_run_sandbox_preserves_caller_cwd_and_cleans_workspace() {
    let Some(nacelle) = require_native_provider_prerequisites() else {
        return;
    };

    let temp = workspace_tempdir("provider-pypi-run-");
    let caller_dir = temp.path().join("caller");
    let index_root = temp.path().join("index");
    let home = workspace_tempdir("provider-pypi-home-");
    let poisoned_path = workspace_tempdir("provider-pypi-path-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&index_root).expect("create index dir");
    write_poison_python_shims(poisoned_path.path());

    let helper_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "demo-provider-pdf-helper",
            version: "0.1.0",
            module_name: "demo_provider_pdf_helper",
            module_files: vec![("__init__.py", "PDF_HELPER = True\n")],
            metadata_lines: Vec::new(),
            console_script_name: None,
            console_script_entrypoint: None,
        },
    );
    let provider_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "demo-provider",
            version: "0.1.0",
            module_name: "demo_provider",
            module_files: vec![("cli.py", demo_provider_cli_source())],
            metadata_lines: vec![
                "Provides-Extra: pdf".to_string(),
                "Requires-Dist: demo-provider-pdf-helper; extra == 'pdf'".to_string(),
            ],
            console_script_name: Some("demo-provider"),
            console_script_entrypoint: Some("demo_provider.cli:main"),
        },
    );
    write_simple_index(
        &index_root,
        &[
            ("demo-provider", vec![provider_wheel.clone()]),
            ("demo-provider-pdf-helper", vec![helper_wheel.clone()]),
        ],
    );
    let server = spawn_static_file_server(index_root.clone());

    let input_path = caller_dir.join("input.txt");
    let output_path = caller_dir.join("output.json");
    fs::write(&input_path, "hello from provider package\n").expect("write input");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--sandbox")
        .arg("--yes")
        .arg("--nacelle")
        .arg(&nacelle)
        .arg("--read")
        .arg("./input.txt")
        .arg("--write")
        .arg("./output.json")
        .arg("pypi:demo-provider[pdf]")
        .arg("--")
        .args(["./input.txt", "-o", "./output.json"])
        .env("HOME", home.path())
        .env(
            "UV_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env(
            "PIP_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env("UV_INSECURE_HOST", "127.0.0.1")
        .env("PATH", prepend_path(poisoned_path.path()))
        .output()
        .expect("run provider-backed sandbox fixture");

    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./input.txt", "-o", "./output.json"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from provider package\n")
    );
    assert_eq!(
        payload["helper_available"].as_bool(),
        Some(true),
        "extras-backed helper dependency should be installed"
    );
    assert_eq!(
        payload["python_version"][0].as_i64(),
        Some(3),
        "provider-backed run must execute under a Python 3 managed runtime"
    );
    assert_eq!(
        payload["python_version"][1].as_i64(),
        Some(11),
        "provider-backed run must execute under the pinned Python 3.11 runtime line"
    );
    assert!(
        !caller_dir.join(".ato").exists(),
        "provider-backed run should not create workspace-local .ato state"
    );

    let provider_runs = provider_runs_root(home.path());
    let remaining = if provider_runs.is_dir() {
        fs::read_dir(&provider_runs)
            .expect("read provider runs dir")
            .next()
            .is_some()
    } else {
        false
    };
    assert!(
        !remaining,
        "provider-backed synthetic workspaces should be cleaned up under {}",
        provider_runs.display()
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_pypi_run_keep_failed_artifacts_retains_workspace_on_watch_error() {
    if !require_provider_materialization_prerequisites() {
        return;
    }

    let temp = workspace_tempdir("provider-pypi-keep-failed-");
    let caller_dir = temp.path().join("caller");
    let index_root = temp.path().join("index");
    let home = workspace_tempdir("provider-pypi-keep-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&index_root).expect("create index dir");

    let provider_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "demo-provider",
            version: "0.1.0",
            module_name: "demo_provider",
            module_files: vec![("cli.py", demo_provider_cli_source())],
            metadata_lines: Vec::new(),
            console_script_name: Some("demo-provider"),
            console_script_entrypoint: Some("demo_provider.cli:main"),
        },
    );
    write_simple_index(
        &index_root,
        &[("demo-provider", vec![provider_wheel.clone()])],
    );
    let server = spawn_static_file_server(index_root.clone());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--watch")
        .arg("--yes")
        .arg("--keep-failed-artifacts")
        .arg("pypi:demo-provider")
        .env("HOME", home.path())
        .env(
            "UV_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env(
            "PIP_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env("UV_INSECURE_HOST", "127.0.0.1")
        .output()
        .expect("run provider-backed keep-failed-artifacts fixture");

    assert!(
        !output.status.success(),
        "watch should fail for provider-backed targets"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Kept failed provider-backed workspace for debugging"),
        "stderr={stderr}"
    );

    let provider_runs = provider_runs_root(home.path());
    let retained = fs::read_dir(&provider_runs)
        .expect("read provider runs dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1, "retained={retained:?}");
    assert!(
        stderr.contains(retained[0].to_string_lossy().as_ref()),
        "stderr should include retained workspace path; stderr={stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn provider_pypi_run_plain_markitdown_surfaces_pdf_extra_hint() {
    let Some(nacelle) = require_native_provider_prerequisites() else {
        return;
    };

    let temp = workspace_tempdir("provider-pypi-markitdown-hint-");
    let caller_dir = temp.path().join("caller");
    let index_root = temp.path().join("index");
    let home = workspace_tempdir("provider-pypi-markitdown-home-");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&index_root).expect("create index dir");

    let helper_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "markitdown-pdf-helper",
            version: "0.1.0",
            module_name: "markitdown_pdf_helper",
            module_files: vec![("__init__.py", "PDF_HELPER = True\n")],
            metadata_lines: Vec::new(),
            console_script_name: None,
            console_script_entrypoint: None,
        },
    );
    let provider_wheel = write_test_wheel(
        &index_root,
        TestWheelSpec {
            package_name: "markitdown",
            version: "0.1.0",
            module_name: "markitdown",
            module_files: vec![("__main__.py", fake_markitdown_cli_source())],
            metadata_lines: vec![
                "Provides-Extra: pdf".to_string(),
                "Requires-Dist: markitdown-pdf-helper; extra == 'pdf'".to_string(),
            ],
            console_script_name: Some("markitdown"),
            console_script_entrypoint: Some("markitdown.__main__:main"),
        },
    );
    write_simple_index(
        &index_root,
        &[
            ("markitdown", vec![provider_wheel.clone()]),
            ("markitdown-pdf-helper", vec![helper_wheel.clone()]),
        ],
    );
    let server = spawn_static_file_server(index_root.clone());

    let input_path = caller_dir.join("input.pdf");
    fs::write(&input_path, "%PDF-1.4\nfixture\n").expect("write fake pdf input");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(&caller_dir)
        .arg("run")
        .arg("--sandbox")
        .arg("--yes")
        .arg("--nacelle")
        .arg(&nacelle)
        .arg("--read")
        .arg("./input.pdf")
        .arg("--write")
        .arg("./out.md")
        .arg("pypi:markitdown")
        .arg("--")
        .args(["./input.pdf", "-o", "./out.md"])
        .env("HOME", home.path())
        .env(
            "UV_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env(
            "PIP_INDEX_URL",
            format!("{}/simple", server.base_url.as_str()),
        )
        .env("UV_INSECURE_HOST", "127.0.0.1")
        .output()
        .expect("run plain markitdown provider fixture");

    if host_limited(&String::from_utf8_lossy(&output.stderr)) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert!(
        !output.status.success(),
        "plain markitdown should fail without [pdf]"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hint: markitdown[pdf] extra may be required for PDF input."),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("Try: ato run pypi:markitdown[pdf] -- ..."),
        "stderr={stderr}"
    );
}
