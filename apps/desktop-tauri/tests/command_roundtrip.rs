//! Command-level round-trip tests.
//!
//! These drive the real command handlers through Tauri's mock runtime so the
//! invoke → Rust command → `ato` CLI path is exercised without a GUI. This is
//! the automated half of the macOS smoke gate; the human half (live WebView
//! on_navigation) remains a manual step. The `ato` binary is resolved from the
//! workspace build and injected via `ATO_DESKTOP_ATO_BIN`.

use std::path::PathBuf;

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets};
use tauri::webview::InvokeRequest;

fn create_app<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::App<R> {
    builder
        .manage(ato_desktop_tauri_lib::host::DesktopHost::new())
        .invoke_handler(tauri::generate_handler![
            ato_desktop_tauri_lib::host::desktop_info,
            ato_desktop_tauri_lib::host::computation_execute,
            ato_desktop_tauri_lib::host::run_cancel,
            ato_desktop_tauri_lib::host::run_inspect,
            ato_desktop_tauri_lib::host::pick_project,
            ato_desktop_tauri_lib::host::open_home,
            ato_desktop_tauri_lib::host::open_web_surface,
        ])
        .build(mock_context(noop_assets()))
        .expect("failed to build app")
}

fn request(cmd: &str, body: serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    }
}

/// Point `ATO_DESKTOP_ATO_BIN` at the workspace-built CLI so the command layer
/// exercises a real binary. CI builds the release CLI before the tauri gate.
fn set_ato_bin() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for candidate in [
        manifest.join("../../target/release/ato"),
        manifest.join("../../target/debug/ato"),
        manifest.join("target/debug/ato"),
    ] {
        if candidate.is_file() {
            // SAFETY: this test runs in its own process; setting the env var before
            // any other thread reads it is safe.
            unsafe { std::env::set_var("ATO_DESKTOP_ATO_BIN", candidate) };
            return;
        }
    }
    panic!("no ato binary found; build ato-cli (debug or release) first");
}

#[test]
fn desktop_info_round_trips_through_the_invoke_layer() {
    let app = create_app(mock_builder());
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let response = get_ipc_response(&webview, request("desktop_info", serde_json::json!({})))
        .expect("desktop_info command failed");
    let value: serde_json::Value = response.deserialize().unwrap();
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["platform"], std::env::consts::OS);
}

#[test]
fn non_main_callers_are_rejected() {
    let app = create_app(mock_builder());
    let webview = tauri::WebviewWindowBuilder::new(&app, "home", Default::default())
        .build()
        .unwrap();
    let response = get_ipc_response(&webview, request("desktop_info", serde_json::json!({})));
    assert!(
        response.is_err(),
        "a non-main window must not invoke native commands"
    );
}

#[test]
fn run_inspect_drives_the_cli_end_to_end() {
    set_ato_bin();
    let app = create_app(mock_builder());
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    // A project with no active Run must come back as `inactive` from the real
    // CLI, proving invoke → command → bundled ato → JSON response.
    let dir = tempfile::tempdir().unwrap();
    let response = get_ipc_response(
        &webview,
        request(
            "run_inspect",
            serde_json::json!({ "project": dir.path().to_str().unwrap() }),
        ),
    )
    .expect("run_inspect command failed");
    let value: serde_json::Value = response.deserialize().unwrap();
    assert_eq!(value["status"], "inactive");
}
