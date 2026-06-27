#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod automation;
mod bridge;
mod bundle_paths;
mod cli_envelope;
mod cli_install;
mod community_api;
mod config;
mod crash;
mod egress_policy;
mod egress_proxy;
mod github_manifest_draft;
mod install_lifecycle_dashboard;
mod ipc;
mod launch_intent;
mod localization;
mod logging;
mod net_client;
mod netd;
mod orchestrator;
mod proc_util;
mod pwa_home;
mod retention;
mod runtime_control_client;
mod runtime_setup;
mod secret_bridge;
mod settings;
mod source_import_api;
mod source_import_runner;
mod source_import_session;
mod state;
mod surface_timing;
mod system_capsule;
mod terminal;
mod userland;
mod webview;
mod webview_init_guard;
mod window;

/// Parse `--jump-action <token>` from argv, returning the token only if it is a
/// recognised lifecycle action. Used to forward taskbar Jump List clicks.
#[cfg(target_os = "windows")]
fn jump_action_arg() -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--jump-action" {
            return args
                .next()
                .filter(|v| crate::window::taskbar::is_known_action(v));
        }
    }
    None
}

fn main() {
    // Must be called before any windows are created so Windows groups
    // taskbar entries correctly and shows "Ato Desktop" instead of the exe name.
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let _ = unsafe {
            SetCurrentProcessExplicitAppUserModelID(windows::core::w!("run.ato.desktop"))
        };
    }

    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ato-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let _log_guard = logging::init_tracing();

    // Taskbar Jump List action (`--jump-action <token>`): forward the lifecycle
    // command to the already-running Desktop over the control pipe and exit,
    // rather than opening a second Desktop. No-op for normal launches.
    #[cfg(target_os = "windows")]
    if let Some(action) = jump_action_arg() {
        if crate::window::taskbar::forward_jump_action(&action) {
            tracing::info!(%action, "jump-action forwarded to running Desktop; exiting");
            return;
        }
        if action == "stop-all" || action == "quit" {
            // Nothing to control — do not spin up a Desktop just to quit it.
            tracing::info!(%action, "jump-action with no running Desktop; nothing to do");
            return;
        }
        tracing::info!(%action, "jump-action with no running Desktop; starting normally");
    }

    // Capture panics to the log file + a crash report + (on Windows) a
    // copyable dialog. Without this, a panic inside GPUI's non-unwinding
    // `open_window` callback aborts the GUI process with no visible message.
    crash::install_panic_hook();

    // On Windows, GPUI renders into a DirectComposition swapchain
    // (WS_EX_NOREDIRECTIONBITMAP). Child Wry/WebView2 HWNDs are not part of
    // that visual tree, so they are occluded and every WebView-backed window
    // paints black/blank (this includes the Start window, which hosts its own
    // child WebView via build_as_child). Disabling DirectComposition makes the
    // WebView2 child windows composite normally while DWM accent transparency
    // (used for the control-bar pill) keeps working. We default it on but
    // honour an explicit override so power users can still opt back into
    // DirectComposition.
    #[cfg(target_os = "windows")]
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_none() {
        // SAFETY: set on the main thread before the GPUI platform is created
        // (which reads this env var). No other thread reads this variable.
        unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
        tracing::info!(
            "Windows: defaulting GPUI_DISABLE_DIRECT_COMPOSITION=1 so WebView2 children composite"
        );
    }

    // WebView2 places its user-data folder next to the executable by default
    // (`<exe>.WebView2`). When ato-desktop is installed to C:\Program Files\Ato\
    // (the MSI default), that location is read-only for a normal user, so
    // WebView2 environment creation fails with E_ACCESSDENIED (0x80070005).
    // Wry's `build_as_child` then returns an error that every window builder
    // `.expect()`s — and because that runs inside GPUI's non-unwinding
    // `open_window` callback, the panic aborts the whole GUI process via the
    // Windows fail-fast path (exit code 0xc0000409) the instant the first
    // WebView-backed window (the Start surface) is opened. The GUI subsystem
    // build has no console and no panic hook, so the user only sees a silent
    // crash. (This never reproduces under `cargo run`, which runs from a
    // writable target/ directory.)
    //
    // Point WebView2 at a per-user writable folder under ~/.ato so the
    // Program Files install works. An explicit override is honored.
    #[cfg(target_os = "windows")]
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        let data_folder = capsule::common::paths::ato_path_or_workspace_tmp("desktop/webview2");
        // Best-effort: WebView2 will create the folder itself, but creating it
        // up front surfaces any permission problem in the log rather than as a
        // later WebView2 failure.
        let _ = std::fs::create_dir_all(&data_folder);
        // SAFETY: set on the main thread before any window / WebView2
        // environment is created (Wry reads this when it builds the first
        // WebView). No other thread reads this variable yet.
        unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &data_folder) };
        tracing::info!(
            ?data_folder,
            "Windows: set WEBVIEW2_USER_DATA_FOLDER to a writable per-user folder"
        );
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        pid = std::process::id(),
        "ato-desktop starting",
    );

    let skip_onboarding = std::env::args().any(|a| a == "--skip-onboarding");

    match crate::orchestrator::resolve_ato_binary() {
        Ok(path) => tracing::info!(?path, "resolved ato binary"),
        Err(error) => tracing::warn!(?error, "could not resolve ato binary at startup"),
    }

    app::run(skip_onboarding);
}
