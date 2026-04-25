mod fail_closed_support;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create workspace .ato/test-scratch");
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
fn maybe_resolve_python_path() -> Option<PathBuf> {
    which::which("python3")
        .ok()
        .or_else(|| which::which("python").ok())
}

#[cfg(unix)]
fn write_single_file_script(path: &Path) {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_path")
    parser.add_argument("-o", "--output", required=True)
    parser.add_argument("--secondary-input")
    parser.add_argument("--secondary-output")
    parser.add_argument("--exit-code", type=int, default=0)
    args = parser.parse_args()

    input_path = Path(args.input_path)
    output_path = Path(args.output)
    payload = {
        "cwd": os.getcwd(),
        "argv": [args.input_path, "-o", args.output],
        "input_exists": input_path.exists(),
        "content": input_path.read_text(encoding="utf-8"),
    }
    if args.secondary_input is not None:
        secondary_input = Path(args.secondary_input)
        payload["secondary_input_exists"] = secondary_input.exists()
        payload["secondary_content"] = secondary_input.read_text(encoding="utf-8")
    if args.secondary_output is not None:
        secondary_output = Path(args.secondary_output)
        payload["secondary_output"] = args.secondary_output
        secondary_output.write_text(json.dumps(payload, ensure_ascii=True), encoding="utf-8")
    output_path.write_text(json.dumps(payload, ensure_ascii=True), encoding="utf-8")
    print(json.dumps(payload, ensure_ascii=True))
    return args.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
"#,
    )
    .expect("write single-file python fixture");
}

#[cfg(unix)]
fn run_single_file_sandbox(
    caller_dir: &Path,
    script_arg: &str,
    nacelle: &Path,
    sandbox_args: &[&str],
    target_args: &[&str],
) -> std::process::Output {
    run_single_file_sandbox_with_runtime_args(
        caller_dir,
        script_arg,
        nacelle,
        &[],
        sandbox_args,
        target_args,
    )
}

#[cfg(unix)]
fn run_single_file_sandbox_with_runtime_args(
    caller_dir: &Path,
    script_arg: &str,
    nacelle: &Path,
    runtime_args: &[&str],
    sandbox_args: &[&str],
    target_args: &[&str],
) -> std::process::Output {
    let home = workspace_tempdir("single-file-home-");
    std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(caller_dir)
        .arg("run")
        .arg("--sandbox")
        .arg("--yes")
        .arg("--nacelle")
        .arg(nacelle)
        .args(runtime_args)
        .args(sandbox_args)
        .arg(script_arg)
        .arg("--")
        .args(target_args)
        .env("HOME", home.path())
        .output()
        .expect("run single-file python sandbox fixture")
}

#[cfg(unix)]
fn run_single_file_host_with_runtime_args(
    caller_dir: &Path,
    script_arg: &str,
    runtime_args: &[&str],
    sandbox_args: &[&str],
    target_args: &[&str],
) -> std::process::Output {
    let home = workspace_tempdir("single-file-host-home-");
    std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .current_dir(caller_dir)
        .arg("run")
        .arg("--dangerously-skip-permissions")
        .arg("--yes")
        .args(runtime_args)
        .args(sandbox_args)
        .arg(script_arg)
        .arg("--")
        .args(target_args)
        .env("HOME", home.path())
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .output()
        .expect("run single-file python host fixture")
}

#[cfg(unix)]
fn assert_failure_or_skip(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if host_limited(&stderr) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={stderr}"
        );
        return false;
    }

    assert!(
        !output.status.success(),
        "expected sandbox command to fail; stdout={}; stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    true
}

#[cfg(unix)]
fn require_native_prerequisites() -> Option<(PathBuf, PathBuf)> {
    let Some(nacelle) = maybe_resolve_test_nacelle_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires nacelle to be available for single_file_python_sandbox_e2e"
        );
        return None;
    };
    let Some(uv) = maybe_resolve_uv_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires uv to be available for single_file_python_sandbox_e2e"
        );
        return None;
    };
    Some((nacelle, uv))
}

#[cfg(unix)]
fn require_host_single_file_prerequisites() -> Option<(PathBuf, PathBuf)> {
    let Some(uv) = maybe_resolve_uv_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires uv to be available for single-file host execution tests"
        );
        return None;
    };
    let Some(python) = maybe_resolve_python_path() else {
        assert!(
            !strict_ci(),
            "strict CI requires python to be available for single-file host execution tests"
        );
        return None;
    };
    Some((uv, python))
}

#[cfg(unix)]
fn setup_single_file_workspace() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let temp = workspace_tempdir("single-file-python-");
    let tool_dir = temp.path().join("tool");
    let caller_dir = temp.path().join("caller");
    fs::create_dir_all(&tool_dir).expect("create tool dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");

    let script_path = tool_dir.join("convert.py");
    write_single_file_script(&script_path);
    (temp, tool_dir, caller_dir, script_path)
}

#[cfg(unix)]
fn assert_symlink_escape_message(output: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("traverses symlink")
            || stderr.contains("symlink")
            || stderr.contains("failed to inspect path component"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn assert_missing_grant_preflight(
    output: &std::process::Output,
    detail: &str,
    raw_path: &str,
    suggestion: &str,
) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains(&format!("Missing {detail} grant for {raw_path}")),
        "stderr={stderr}"
    );
    assert!(stderr.contains("Try:"), "stderr={stderr}");
    assert!(stderr.contains("E999"), "stderr={stderr}");
    assert!(stderr.contains("help:"), "stderr={stderr}");
    assert!(stderr.contains(suggestion), "stderr={stderr}");
    assert!(
        !stdout.contains("PHASE execute RUN"),
        "preflight failure should not reach execute phase; stdout={stdout}"
    );
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

    assert!(output.status.success(), "stderr={stderr}");
    true
}

#[cfg(unix)]
fn assert_exit_code_or_skip(output: &std::process::Output, expected: i32) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if host_limited(&stderr) {
        assert!(
            !strict_ci(),
            "strict CI requires working native sandbox; stderr={stderr}"
        );
        return false;
    }

    assert_eq!(output.status.code(), Some(expected), "stderr={stderr}");
    true
}

#[cfg(unix)]
fn load_output_json(path: &Path, search_root: &Path, output: &std::process::Output) -> Value {
    for _ in 0..100 {
        if let Ok(raw) = fs::read(path) {
            return serde_json::from_slice(&raw).expect("parse output json");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let mut relocated = Vec::new();
    for entry in walkdir::WalkDir::new(search_root) {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_name() == "output.txt" {
            relocated.push(entry.path().display().to_string());
        }
    }

    let raw = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "read output json: {error}; expected={}; found={:?}; stdout={}; stderr={}",
            path.display(),
            relocated,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    serde_json::from_slice(&raw).expect("parse output json")
}

#[cfg(unix)]
fn assert_no_nested_workspace_tmp(root: &Path) {
    let nested = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|entry| {
            let components = entry
                .path()
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            components.windows(4).any(|window| {
                window[0] == ".ato"
                    && window[1] == "tmp"
                    && window[2] == ".ato"
                    && window[3] == "tmp"
            })
        });
    assert!(
        !nested,
        "found nested .ato/tmp recursion under {}",
        root.display()
    );
}

#[cfg(unix)]
fn assert_no_workspace_state_dir(root: &Path) {
    assert!(
        !root.join(".ato").exists(),
        "unexpected workspace-local .ato created under {}",
        root.display()
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_preserves_relative_read_write_and_caller_cwd() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello from caller cwd\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./input.txt", "--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&caller_dir.join("output.txt"), temp.path(), &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./input.txt", "-o", "./output.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(payload["content"].as_str(), Some("hello from caller cwd\n"));
    assert_no_workspace_state_dir(&caller_dir);
    assert_no_nested_workspace_tmp(temp.path());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_markitdown_relative_output_stays_in_caller_workspace() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let input_path = caller_dir.join("phd-thesis.pdf");
    let output_path = caller_dir.join("phd-thesis.rel.md");
    fs::write(&input_path, "fake pdf bytes for markitdown\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &[
            "--read",
            "./phd-thesis.pdf",
            "--write",
            "./phd-thesis.rel.md",
        ],
        &["./phd-thesis.pdf", "-o", "./phd-thesis.rel.md"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./phd-thesis.pdf", "-o", "./phd-thesis.rel.md"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("fake pdf bytes for markitdown\n")
    );
    assert!(
        output_path.exists(),
        "relative markdown output should stay in caller workspace"
    );
    assert_no_workspace_state_dir(&caller_dir);
    assert_no_nested_workspace_tmp(temp.path());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_markitdown_relative_output_stays_in_caller_workspace_with_cwd_override(
) {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let input_path = caller_dir.join("phd-thesis.pdf");
    let output_path = caller_dir.join("phd-thesis.cwd.md");
    fs::write(&input_path, "fake pdf bytes for markitdown\n").expect("write input");

    let override_arg = caller_dir.display().to_string();
    let output = run_single_file_sandbox_with_runtime_args(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--cwd", &override_arg],
        &[
            "--read",
            "./phd-thesis.pdf",
            "--write",
            "./phd-thesis.cwd.md",
        ],
        &["./phd-thesis.pdf", "-o", "./phd-thesis.cwd.md"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./phd-thesis.pdf", "-o", "./phd-thesis.cwd.md"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("fake pdf bytes for markitdown\n")
    );
    assert!(
        output_path.exists(),
        "relative markdown output should stay in caller workspace with explicit cwd override"
    );
    assert_no_workspace_state_dir(&caller_dir);
    assert_no_nested_workspace_tmp(temp.path());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_host_execution_honors_effective_cwd_for_relative_output() {
    let Some((_uv, _python)) = require_host_single_file_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let override_dir = temp.path().join("override-cwd");
    fs::create_dir_all(&override_dir).expect("create override dir");
    fs::write(caller_dir.join("input.txt"), "hello from caller cwd\n").expect("write caller input");
    fs::write(override_dir.join("input.txt"), "hello from effective cwd\n")
        .expect("write override input");

    let override_arg = override_dir.display().to_string();
    let output = run_single_file_host_with_runtime_args(
        &caller_dir,
        "../tool/convert.py",
        &["--cwd", &override_arg],
        &["--read", "./input.txt", "--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );

    assert!(
        output.status.success(),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let payload = load_output_json(&override_dir.join("output.txt"), temp.path(), &output);
    let expected_cwd = override_dir
        .canonicalize()
        .expect("canonicalize expected override cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from effective cwd\n")
    );
    assert!(!caller_dir.join("output.txt").exists());
    assert_no_nested_workspace_tmp(temp.path());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_treats_plain_and_dot_relative_paths_equivalently() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello equivalent spelling\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "input.txt", "--write", "output.txt"],
        &["input.txt", "-o", "output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&caller_dir.join("output.txt"), temp.path(), &output);
    assert_eq!(
        payload["argv"],
        serde_json::json!(["input.txt", "-o", "output.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello equivalent spelling\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_read_write_directory_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let granted_dir = caller_dir.join("granted");
    fs::create_dir_all(&granted_dir).expect("create granted dir");
    fs::write(granted_dir.join("input.txt"), "hello from granted dir\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read-write", "./granted"],
        &["./granted/input.txt", "-o", "./granted/output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&granted_dir.join("output.txt"), temp.path(), &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./granted/input.txt", "-o", "./granted/output.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from granted dir\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_absolute_paths() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let input_path = caller_dir.join("input.txt");
    let output_path = caller_dir.join("output.txt");
    fs::write(&input_path, "hello from absolute path\n").expect("write input");

    let input_arg = input_path.display().to_string();
    let output_arg = output_path.display().to_string();
    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", &input_arg, "--write", &output_arg],
        &[&input_arg, "-o", &output_arg],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    let expected_cwd = caller_dir
        .canonicalize()
        .expect("canonicalize expected caller cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!([input_arg, "-o", output_arg])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from absolute path\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_cwd_override() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let override_dir = temp.path().join("override");
    fs::create_dir_all(&override_dir).expect("create override dir");
    fs::write(override_dir.join("input.txt"), "hello from override cwd\n").expect("write input");

    let override_arg = override_dir.display().to_string();
    let output = run_single_file_sandbox_with_runtime_args(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--cwd", &override_arg],
        &["--read", "./input.txt", "--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&override_dir.join("output.txt"), temp.path(), &output);
    let expected_cwd = override_dir
        .canonicalize()
        .expect("canonicalize expected override cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./input.txt", "-o", "./output.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from override cwd\n")
    );
    assert!(!caller_dir.join("output.txt").exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_resolves_relative_grants_and_args_from_cwd_override() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let override_dir = temp.path().join("override-cwd");
    fs::create_dir_all(&override_dir).expect("create override dir");
    fs::write(caller_dir.join("input.txt"), "hello from caller cwd\n").expect("write caller input");
    fs::write(override_dir.join("input.txt"), "hello from effective cwd\n")
        .expect("write override input");

    let override_arg = override_dir.display().to_string();
    let output = run_single_file_sandbox_with_runtime_args(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--cwd", &override_arg],
        &["--read", "./input.txt", "--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&override_dir.join("output.txt"), temp.path(), &output);
    let expected_cwd = override_dir
        .canonicalize()
        .expect("canonicalize expected override cwd");
    assert_eq!(
        payload["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from effective cwd\n")
    );
    assert!(!caller_dir.join("output.txt").exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_unions_directory_and_file_grants() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let shared_dir = caller_dir.join("shared");
    let output_path = shared_dir.join("output.txt");
    fs::create_dir_all(&shared_dir).expect("create shared dir");
    fs::write(shared_dir.join("input.txt"), "hello from union grants\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./shared", "--write", "./shared/output.txt"],
        &["./shared/input.txt", "-o", "./shared/output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello from union grants\n")
    );
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./shared/input.txt", "-o", "./shared/output.txt"])
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_mixed_file_and_directory_grant_matrix() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let docs_dir = caller_dir.join("docs");
    let cache_dir = caller_dir.join("cache");
    fs::create_dir_all(&docs_dir).expect("create docs dir");
    fs::create_dir_all(&cache_dir).expect("create cache dir");
    fs::write(docs_dir.join("input.txt"), "hello mixed matrix\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &[
            "--read",
            "./docs",
            "--write",
            "./docs/result.json",
            "--write",
            "./cache/aux.json",
        ],
        &[
            "./docs/input.txt",
            "-o",
            "./docs/result.json",
            "--secondary-input",
            "./docs/input.txt",
            "--secondary-output",
            "./cache/aux.json",
        ],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&docs_dir.join("result.json"), temp.path(), &output);
    let aux_payload = load_output_json(&cache_dir.join("aux.json"), temp.path(), &output);
    assert_eq!(payload["content"].as_str(), Some("hello mixed matrix\n"));
    assert_eq!(
        payload["secondary_content"].as_str(),
        Some("hello mixed matrix\n")
    );
    assert_eq!(
        aux_payload["secondary_output"].as_str(),
        Some("./cache/aux.json")
    );
    assert_eq!(
        aux_payload["secondary_content"].as_str(),
        Some("hello mixed matrix\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_surfaces_precise_missing_write_grant_error() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (_temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello missing write grant\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./input.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    assert_missing_grant_preflight(&output, "write", "./output.txt", "--write ./output.txt");
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_write_without_write_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello from read only\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./input.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("permission denied")
            || stderr.contains("operation not permitted")
            || stderr.contains("sandbox")
            || stderr.contains("denied"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_read_without_read_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello from write only\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("permission denied")
            || stderr.contains("operation not permitted")
            || stderr.contains("sandbox")
            || stderr.contains("denied")
            || stderr.contains("no such file"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_directory_write_without_write_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let readonly_dir = caller_dir.join("readonly-dir");
    fs::create_dir_all(&readonly_dir).expect("create readonly dir");
    fs::write(readonly_dir.join("input.txt"), "hello from readonly dir\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./readonly-dir"],
        &[
            "./readonly-dir/input.txt",
            "-o",
            "./readonly-dir/output.txt",
        ],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!readonly_dir.join("output.txt").exists());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("permission denied")
            || stderr.contains("operation not permitted")
            || stderr.contains("sandbox")
            || stderr.contains("denied"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_directory_read_without_read_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let writeonly_dir = caller_dir.join("writeonly-dir");
    fs::create_dir_all(&writeonly_dir).expect("create writeonly dir");
    fs::write(
        writeonly_dir.join("input.txt"),
        "hello from writeonly dir\n",
    )
    .expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--write", "./writeonly-dir"],
        &[
            "./writeonly-dir/input.txt",
            "-o",
            "./writeonly-dir/output.txt",
        ],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!writeonly_dir.join("output.txt").exists());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("permission denied")
            || stderr.contains("operation not permitted")
            || stderr.contains("sandbox")
            || stderr.contains("denied")
            || stderr.contains("no such file"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_symlink_escape_on_read_path() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let granted_dir = caller_dir.join("granted");
    let outside_dir = temp.path().join("outside");
    fs::create_dir_all(&granted_dir).expect("create granted dir");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("secret.txt"), "top secret\n").expect("write secret input");
    unix_fs::symlink(&outside_dir, granted_dir.join("escape")).expect("create escape symlink");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &[
            "--read",
            "./granted/escape/secret.txt",
            "--write",
            "./output.txt",
        ],
        &["./granted/escape/secret.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    assert_symlink_escape_message(&output);
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_symlink_escape_on_write_path() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let granted_dir = caller_dir.join("granted");
    let outside_dir = temp.path().join("outside");
    fs::create_dir_all(&granted_dir).expect("create granted dir");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(granted_dir.join("input.txt"), "hello from granted dir\n").expect("write input");
    unix_fs::symlink(&outside_dir, granted_dir.join("escape")).expect("create escape symlink");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &[
            "--read",
            "./granted/input.txt",
            "--write",
            "./granted/escape/output.txt",
        ],
        &["./granted/input.txt", "-o", "./granted/escape/output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!outside_dir.join("output.txt").exists());
    assert_symlink_escape_message(&output);
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_rejects_normalized_path_escape() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let granted_dir = caller_dir.join("granted");
    let outside_path = caller_dir.join("secret.txt");
    fs::create_dir_all(granted_dir.join("sub")).expect("create granted subdir");
    fs::write(&outside_path, "top secret\n").expect("write outside input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./granted", "--write", "./output.txt"],
        &["./granted/sub/../../secret.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    assert_missing_grant_preflight(
        &output,
        "read",
        "./granted/sub/../../secret.txt",
        "--read ./granted/sub/../../secret.txt",
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_creates_nonexistent_output_with_exact_write_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let input_path = caller_dir.join("input.txt");
    let output_path = caller_dir.join("new-output.txt");
    fs::write(&input_path, "hello create semantics\n").expect("write input");
    assert!(!output_path.exists(), "output should start absent");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./input.txt", "--write", "./new-output.txt"],
        &["./input.txt", "-o", "./new-output.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    assert!(output_path.exists(), "output should be created");
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./input.txt", "-o", "./new-output.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(
        payload["content"].as_str(),
        Some("hello create semantics\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_surfaces_preflight_failure_shape_for_missing_read_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(caller_dir.join("input.txt"), "hello preflight\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("output.txt").exists());
    assert_missing_grant_preflight(&output, "read", "./input.txt", "--read ./input.txt");
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_markitdown_relative_input_requires_read_grant() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    fs::write(
        caller_dir.join("phd-thesis.pdf"),
        "fake pdf bytes for markitdown\n",
    )
    .expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--write", "./phd-thesis.rel.md"],
        &["./phd-thesis.pdf", "-o", "./phd-thesis.rel.md"],
    );
    if !assert_failure_or_skip(&output) {
        return;
    }

    assert!(!caller_dir.join("phd-thesis.rel.md").exists());
    assert_missing_grant_preflight(
        &output,
        "read",
        "./phd-thesis.pdf",
        "--read ./phd-thesis.pdf",
    );
    assert!(temp.path().exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_propagates_target_exit_code() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let output_path = caller_dir.join("output.txt");
    fs::write(caller_dir.join("input.txt"), "hello exit code\n").expect("write input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", "./input.txt", "--write", "./output.txt"],
        &["./input.txt", "-o", "./output.txt", "--exit-code", "17"],
    );
    if !assert_exit_code_or_skip(&output, 17) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    assert_eq!(payload["content"].as_str(), Some("hello exit code\n"));
    assert!(
        output_path.exists(),
        "output should still be materialized before exit"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_spaces_and_unicode_paths() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let input_path = caller_dir.join("my file 日本語.txt");
    let output_path = caller_dir.join("out file 日本語.txt");
    fs::write(&input_path, "hello unicode path\n").expect("write unicode input");

    let input_arg = "./my file 日本語.txt";
    let output_arg = "./out file 日本語.txt";
    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read", input_arg, "--write", output_arg],
        &[input_arg, "-o", output_arg],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&output_path, temp.path(), &output);
    assert_eq!(
        payload["argv"],
        serde_json::json!([input_arg, "-o", output_arg])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(payload["content"].as_str(), Some("hello unicode path\n"));
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_multiple_inputs_and_outputs() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let inputs_dir = caller_dir.join("inputs");
    let outputs_dir = caller_dir.join("outputs");
    fs::create_dir_all(&inputs_dir).expect("create inputs dir");
    fs::create_dir_all(&outputs_dir).expect("create outputs dir");
    fs::write(inputs_dir.join("one.txt"), "first input\n").expect("write first input");
    fs::write(inputs_dir.join("two.txt"), "second input\n").expect("write second input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &[
            "--read",
            "./inputs",
            "--write",
            "./outputs/one.json",
            "--write",
            "./outputs/two.json",
        ],
        &[
            "./inputs/one.txt",
            "-o",
            "./outputs/one.json",
            "--secondary-input",
            "./inputs/two.txt",
            "--secondary-output",
            "./outputs/two.json",
        ],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let first_payload = load_output_json(&outputs_dir.join("one.json"), temp.path(), &output);
    let second_payload = load_output_json(&outputs_dir.join("two.json"), temp.path(), &output);
    assert_eq!(first_payload["content"].as_str(), Some("first input\n"));
    assert_eq!(
        first_payload["secondary_content"].as_str(),
        Some("second input\n")
    );
    assert_eq!(
        first_payload["secondary_input_exists"].as_bool(),
        Some(true)
    );
    assert_eq!(second_payload["content"].as_str(), Some("first input\n"));
    assert_eq!(
        second_payload["secondary_content"].as_str(),
        Some("second input\n")
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn single_file_python_sandbox_supports_in_place_read_write() {
    let Some((nacelle, _uv)) = require_native_prerequisites() else {
        return;
    };

    let (temp, _tool_dir, caller_dir, _script_path) = setup_single_file_workspace();
    let inplace_path = caller_dir.join("inplace.txt");
    fs::write(&inplace_path, "hello in place\n").expect("write inplace input");

    let output = run_single_file_sandbox(
        &caller_dir,
        "../tool/convert.py",
        &nacelle,
        &["--read-write", "./inplace.txt"],
        &["./inplace.txt", "-o", "./inplace.txt"],
    );
    if !assert_success_or_skip(&output) {
        return;
    }

    let payload = load_output_json(&inplace_path, temp.path(), &output);
    assert_eq!(
        payload["argv"],
        serde_json::json!(["./inplace.txt", "-o", "./inplace.txt"])
    );
    assert_eq!(payload["input_exists"].as_bool(), Some(true));
    assert_eq!(payload["content"].as_str(), Some("hello in place\n"));
}
