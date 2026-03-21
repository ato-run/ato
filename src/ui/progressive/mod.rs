use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use capsule_core::execution_plan::guard::ExecutorKind;
use cliclack::{confirm, intro, log, note, outro, outro_cancel, spinner, ProgressBar};
use console::style;

use crate::preview::{DerivedExecutionPlan, PreviewPromotionEligibility, PreviewSession};

const PATH_WRAP_WIDTH: usize = 72;
static FLOW_ACTIVE: AtomicBool = AtomicBool::new(false);
const ATO_LOGO: &str = r#"
 █████╗ ████████╗ ██████╗ 
██╔══██╗╚══██╔══╝██╔═══██╗
███████║   ██║   ██║   ██║
██╔══██║   ██║   ██║   ██║
██║  ██║   ██║   ╚██████╔╝
╚═╝  ╚═╝   ╚═╝    ╚═════╝ 
"#;

pub fn can_use_progressive_ui(json_mode: bool) -> bool {
    !json_mode && io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub fn begin_flow() -> Result<()> {
    if FLOW_ACTIVE.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    println!("{}", style(ATO_LOGO).cyan().bold());
    intro(style(" ato ").black().on_cyan()).context("failed to render TUI intro")?;
    Ok(())
}

pub fn is_flow_active() -> bool {
    FLOW_ACTIVE.load(Ordering::SeqCst)
}

pub fn show_run_intro(source: &str) -> Result<()> {
    begin_flow()?;
    log::step(format!("Source: {}", style(source).cyan()))
        .context("failed to render TUI source step")?;
    Ok(())
}

pub fn show_step(message: impl AsRef<str>) -> Result<()> {
    log::step(message.as_ref()).context("failed to render TUI step")?;
    Ok(())
}

pub fn show_success(message: impl AsRef<str>) -> Result<()> {
    log::success(message.as_ref()).context("failed to render TUI success")?;
    Ok(())
}

pub fn show_warning(message: impl AsRef<str>) -> Result<()> {
    log::warning(message.as_ref()).context("failed to render TUI warning")?;
    Ok(())
}

pub fn show_note(title: impl AsRef<str>, body: impl AsRef<str>) -> Result<()> {
    note(title.as_ref(), body.as_ref()).context("failed to render TUI note")?;
    Ok(())
}

pub fn show_cancel(message: impl AsRef<str>) -> Result<()> {
    outro_cancel(message.as_ref()).context("failed to render TUI cancellation")?;
    Ok(())
}

pub fn show_outro(message: impl AsRef<str>) -> Result<()> {
    outro(message.as_ref()).context("failed to render TUI outro")?;
    Ok(())
}

pub fn confirm_action(prompt: &str, default_yes: bool) -> Result<bool> {
    let mut interaction = confirm(prompt).initial_value(default_yes);
    interaction
        .interact()
        .context("failed to read interactive confirmation")
}

pub fn confirm_with_fallback(prompt: &str, default_yes: bool, use_tui: bool) -> Result<bool> {
    if use_tui {
        return confirm_action(prompt, default_yes);
    }

    eprint!("{prompt}");
    io::stderr().flush().context("failed to flush prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read interactive input")?;
    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

pub fn start_spinner(message: &str) -> ProgressBar {
    let progress = spinner();
    progress.start(message);
    progress
}

pub fn render_preview_plan(session: &PreviewSession) -> Result<()> {
    show_note(
        "Derived Execution Plan",
        format_preview_plan(&session.derived_plan),
    )
}

pub fn render_security_context(
    executor_kind: ExecutorKind,
    host_fallback_requested: bool,
    dangerously_skip_permissions: bool,
    port: Option<u16>,
) -> Result<()> {
    let exposed = port
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string());
    let (filesystem, isolation) = if host_fallback_requested {
        (
            "Isolated Host Temp Dir".to_string(),
            "None (Host Fallback)".to_string(),
        )
    } else if dangerously_skip_permissions {
        (
            "Host Working Dir".to_string(),
            "None (Unsafe Host Mode)".to_string(),
        )
    } else {
        match executor_kind {
            ExecutorKind::Native => (
                "Read-Only Host Mounts".to_string(),
                "Nacelle Sandbox".to_string(),
            ),
            ExecutorKind::Deno => (
                "Deno Read Allowlist".to_string(),
                "Deno Permission Model".to_string(),
            ),
            ExecutorKind::NodeCompat => (
                "Compat Read Allowlist".to_string(),
                "Deno Compat Permissions".to_string(),
            ),
            ExecutorKind::Wasm => ("Runtime Managed".to_string(), "Wasm Runtime".to_string()),
            ExecutorKind::WebStatic => (
                "Serve Dir Allowlist".to_string(),
                "Local Static Server".to_string(),
            ),
        }
    };
    let body = format!(
        "           {:<14} {:<24} {:<24}\nPreview    {:<14} {:<24} {:<24}\n           {:<14} {:<24} {:<24}",
        "Network",
        "Filesystem",
        "Isolation",
        format!("Exposed: {}", style(exposed).green()),
        filesystem,
        isolation,
        "Outbound: Yes",
        "tmpfs: /tmp",
        "",
    );
    show_note("Sandbox Security Context", body)
}

pub fn render_host_fallback_warning() -> Result<()> {
    let body = format!(
        "{}\n\n{}\n{}",
        style("This capsule requires features not fully supported by standard Nacelle.")
            .yellow()
            .bold(),
        "ato must fallback to your host environment to run this target.",
        style("- Sandbox: DISABLED (isolated temp directory only)\n- Privacy: Restricted (HOME and cache remain masked where possible)")
            .red()
    );
    show_note("Compatibility Alert", body)
}

pub fn render_execution_consent_summary(summary: &str) -> Result<()> {
    show_note("Execution Plan Permissions", summary)
}

pub fn render_promotion_summary(eligibility: &PreviewPromotionEligibility) -> Result<()> {
    let body = match eligibility {
        PreviewPromotionEligibility::Eligible => {
            "Preview result can be promoted into the persistent store after validation.".to_string()
        }
        PreviewPromotionEligibility::RequiresManualReview => {
            "Preview completed, but promotion requires manual review before install.".to_string()
        }
        PreviewPromotionEligibility::Blocked => {
            "Preview completed, but promotion is blocked for this execution plan.".to_string()
        }
    };
    show_note("Promotion Readiness", body)
}

pub fn render_generated_manifest_preview(manifest_path: &Path, preview_toml: &str) -> Result<()> {
    show_note(
        "Generated Capsule Manifest",
        format_generated_manifest_preview(manifest_path, preview_toml),
    )
}

pub fn render_manual_review_required(
    manifest_path: &Path,
    failure_reason: &str,
    warnings: &[String],
) -> Result<()> {
    let warning_text = if warnings.is_empty() {
        "None".to_string()
    } else {
        format!("- {}", warnings.join("\n- "))
    };
    show_note(
        "Manual Review Required",
        format!(
            "Reason       : {}\nManifest     :\n{}\nNext Steps   : Review the generated target settings before retrying.\nWarnings     : {}",
            failure_reason,
            indent_lines(&wrap_path(manifest_path), 2),
            warning_text,
        ),
    )
}

fn format_preview_plan(plan: &DerivedExecutionPlan) -> String {
    let runtime = plan.runtime.as_deref().unwrap_or("unknown");
    let driver = plan.driver.as_deref().unwrap_or("unknown");
    let runtime_version = plan.resolved_runtime_version.as_deref().unwrap_or("auto");
    let port = plan
        .resolved_port
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let lock_files = if plan.resolved_lock_files.is_empty() {
        "None".to_string()
    } else {
        plan.resolved_lock_files
            .iter()
            .map(|value| value.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let warnings = if plan.warnings.is_empty() {
        "None".to_string()
    } else {
        format!("- {}", plan.warnings.join("\n- "))
    };

    format!(
        "Runtime      : {}\nDriver       : {}\nVersion      : {}\nPort         : {}\nLock Files   : {}\nWarnings     : {}",
        style(runtime).cyan(),
        style(driver).cyan(),
        style(runtime_version).yellow(),
        style(port).green(),
        lock_files,
        warnings
    )
}

fn format_generated_manifest_preview(manifest_path: &Path, preview_toml: &str) -> String {
    let parsed = toml::from_str::<toml::Value>(preview_toml).ok();
    let name = parsed
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("unknown");
    let runtime = parsed
        .as_ref()
        .and_then(|value| value.get("runtime"))
        .and_then(toml::Value::as_str)
        .unwrap_or("unknown");
    let run = parsed
        .as_ref()
        .and_then(|value| value.get("run"))
        .and_then(toml::Value::as_str)
        .unwrap_or("n/a");
    let port = parsed
        .as_ref()
        .and_then(|value| value.get("port"))
        .and_then(toml::Value::as_integer)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let include = parsed
        .as_ref()
        .and_then(|value| value.get("pack"))
        .and_then(|value| value.get("include"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "None".to_string());

    format!(
        "Name         : {}\nRuntime      : {}\nRun          : {}\nPort         : {}\nInclude      : {}\nManifest     :\n{}",
        style(name).cyan(),
        style(runtime).yellow(),
        run,
        style(port).green(),
        include,
        indent_lines(&wrap_path(manifest_path), 2),
    )
}

pub fn format_path_for_note(path: &Path) -> String {
    indent_lines(&wrap_path(path), 2)
}

fn wrap_path(path: &Path) -> Vec<String> {
    let display = abbreviate_home(path.display().to_string());
    wrap_path_str(&display)
}

fn wrap_path_str(path: &str) -> Vec<String> {
    if path.len() <= PATH_WRAP_WIDTH {
        return vec![path.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for segment in path.split('/') {
        let piece = if current.is_empty() {
            segment.to_string()
        } else {
            format!("/{segment}")
        };
        if !current.is_empty() && current.len() + piece.len() > PATH_WRAP_WIDTH {
            lines.push(current);
            current = segment.to_string();
        } else {
            current.push_str(&piece);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn indent_lines(lines: &[String], spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    lines
        .iter()
        .map(|line| format!("{}{}", indent, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn abbreviate_home(path: String) -> String {
    let Some(home) = dirs::home_dir() else {
        return path;
    };
    let home = home.display().to_string();
    if path == home {
        return "~".to_string();
    }
    if let Some(stripped) = path.strip_prefix(&(home + "/")) {
        return format!("~/{stripped}");
    }
    path
}
