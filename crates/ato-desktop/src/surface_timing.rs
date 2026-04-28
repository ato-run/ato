//! `SURFACE-TIMING` — Desktop-side analogue of the CLI `PHASE-TIMING`
//! infrastructure (RFC: SURFACE_MATERIALIZATION, Phase 0 / PR 3).
//!
//! Phase 0 is intentionally **measurement only**. This module:
//!   * gates emission on `ATO_SURFACE_TIMING=1` env (off by default so
//!     production stderr stays clean);
//!   * provides an RAII `SurfaceStageTimer` that emits a one-line
//!     `SURFACE-TIMING phase=execute stage=<name> state=<ok|fail>
//!     elapsed_ms=<n>` when dropped;
//!   * carries optional debug extras (`session_id`, `partition_id`,
//!     `route_key`) so retention design can verify partition stability
//!     across pane close → reopen (RFC §3.3 precondition);
//!   * exposes a free-standing `emit_stage` for ad-hoc points (e.g. the
//!     `total` line written from outside any single timer's scope).
//!
//! No code path outside this module should depend on whether `SURFACE-
//! TIMING` is enabled — the timer's `Drop` is a no-op when off.

use std::time::Instant;

const ENV_TOGGLE: &str = "ATO_SURFACE_TIMING";

pub(crate) fn enabled() -> bool {
    match std::env::var(ENV_TOGGLE) {
        Ok(value) => {
            let trimmed = value.trim();
            !trimmed.is_empty() && !matches!(trimmed, "0" | "false" | "off" | "no")
        }
        Err(_) => false,
    }
}

/// Optional debug extras attached to a `SURFACE-TIMING` line. Used to
/// answer the §3.3 precondition: is `partition_id` stable across a
/// `(session_id, partition_id)` pane close → reopen?
///
/// `since_click_ms` is the user-perceived time anchor: for
/// instant-marker stages (`navigation_*`, `first_visible_signal`)
/// where `elapsed_ms` is meaningless (the stage has no duration),
/// `since_click_ms` carries the wall-clock distance from the click
/// handler entry — that's the number Phase 0 wants.
#[derive(Default, Clone, Debug)]
pub(crate) struct SurfaceExtras {
    pub session_id: Option<String>,
    pub partition_id: Option<String>,
    pub route_key: Option<String>,
    pub since_click_ms: Option<u64>,
}

impl SurfaceExtras {
    pub fn with_session_id(mut self, value: impl Into<String>) -> Self {
        self.session_id = Some(value.into());
        self
    }

    pub fn with_partition_id(mut self, value: impl Into<String>) -> Self {
        self.partition_id = Some(value.into());
        self
    }

    pub fn with_route_key(mut self, value: impl Into<String>) -> Self {
        self.route_key = Some(value.into());
        self
    }

    pub fn with_since_click_ms(mut self, value: u64) -> Self {
        self.since_click_ms = Some(value);
        self
    }

    fn render(&self) -> String {
        let mut parts = String::new();
        if let Some(value) = &self.session_id {
            parts.push_str(&format!(" session_id={:?}", value));
        }
        if let Some(value) = &self.partition_id {
            parts.push_str(&format!(" partition_id={:?}", value));
        }
        if let Some(value) = &self.route_key {
            parts.push_str(&format!(" route_key={:?}", value));
        }
        if let Some(value) = self.since_click_ms {
            parts.push_str(&format!(" since_click_ms={}", value));
        }
        parts
    }
}

/// Emit a single `SURFACE-TIMING` line. No-op when the env toggle is
/// off. Format mirrors the CLI `PHASE-TIMING` lines so jq / grep
/// pipelines can be reused: every line starts with `SURFACE-TIMING`,
/// fields are space-separated `key=value`, and the `error` field (when
/// present) uses `{:?}` (Rust `Debug`) so multi-line payloads stay on
/// one line.
pub(crate) fn emit_stage(
    stage: &str,
    state: &str,
    elapsed_ms: u64,
    error: Option<&str>,
    extras: &SurfaceExtras,
) {
    if !enabled() {
        return;
    }
    let mut line = format!(
        "SURFACE-TIMING stage={} state={} elapsed_ms={}",
        stage, state, elapsed_ms
    );
    line.push_str(&extras.render());
    if let Some(message) = error {
        let truncated: String = message.chars().take(200).collect();
        let one_line = truncated.replace('\n', " ");
        line.push_str(&format!(" error={:?}", one_line));
    }
    eprintln!("{}", line);
}

/// Emit the click → ... summary line. `total_ms` should be the wall-
/// clock time from `click_start` to the moment the user-visible signal
/// fired (whichever Phase 0 / Phase 3 metric is appropriate).
pub(crate) fn emit_total(total_ms: u64, result_kind: &str, extras: &SurfaceExtras) {
    if !enabled() {
        return;
    }
    let mut line = format!(
        "SURFACE-TIMING total elapsed_ms={} result_kind={}",
        total_ms, result_kind
    );
    line.push_str(&extras.render());
    eprintln!("{}", line);
}

/// RAII timer that emits one `SURFACE-TIMING stage=<name>` line on
/// drop. By default it emits `state=ok`; call `mark_fail` from the
/// error path to record `state=fail` (and an optional error message).
///
/// The timer is cheap when `ATO_SURFACE_TIMING` is unset — it still
/// captures `Instant::now` at construction (a single syscall on
/// macOS / Linux) but skips formatting and the eprintln on drop.
pub(crate) struct SurfaceStageTimer {
    stage: &'static str,
    started: Instant,
    state: &'static str,
    error: Option<String>,
    extras: SurfaceExtras,
    finished: bool,
}

impl SurfaceStageTimer {
    pub fn start(stage: &'static str) -> Self {
        Self {
            stage,
            started: Instant::now(),
            state: "ok",
            error: None,
            extras: SurfaceExtras::default(),
            finished: false,
        }
    }

    pub fn with_extras(mut self, extras: SurfaceExtras) -> Self {
        self.extras = extras;
        self
    }

    pub fn finish_ok(mut self) {
        self.finished = true;
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        emit_stage(self.stage, "ok", elapsed_ms, None, &self.extras);
    }

    #[allow(dead_code)] // Wired by Phase 1+ when error paths can be timed precisely.
    pub fn finish_fail(mut self, error: &str) {
        self.finished = true;
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        emit_stage(self.stage, "fail", elapsed_ms, Some(error), &self.extras);
    }
}

impl Drop for SurfaceStageTimer {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // The timer was dropped without explicit finish — emit whatever
        // state was last set (default `ok`) so a panic / early-return
        // path still produces a line.
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        emit_stage(
            self.stage,
            self.state,
            elapsed_ms,
            self.error.as_deref(),
            &self.extras,
        );
    }
}

/// Wall-clock anchor for the `total` line. Capture this at the click
/// handler entry and pass it to `emit_total` once the user-visible
/// signal has fired (or any other terminal point Phase 0 chooses to
/// measure end-to-end against).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClickOrigin {
    started: Instant,
}

impl ClickOrigin {
    pub fn now() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    pub fn elapsed_ms(self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extras_render_skips_unset_fields() {
        let extras = SurfaceExtras::default();
        assert_eq!(extras.render(), "");
    }

    #[test]
    fn extras_render_quotes_values_for_grep_friendliness() {
        let extras = SurfaceExtras::default()
            .with_session_id("ato-desktop-session-12345")
            .with_partition_id("pane-7")
            .with_route_key("local:/foo/bar");
        let rendered = extras.render();
        assert!(rendered.contains("session_id=\"ato-desktop-session-12345\""));
        assert!(rendered.contains("partition_id=\"pane-7\""));
        assert!(rendered.contains("route_key=\"local:/foo/bar\""));
    }

    #[test]
    fn extras_render_emits_since_click_ms_unquoted() {
        let extras = SurfaceExtras::default().with_since_click_ms(2473);
        let rendered = extras.render();
        // Numeric — bare value so jq / awk can parse without strip.
        assert!(rendered.contains("since_click_ms=2473"));
        assert!(!rendered.contains("since_click_ms=\""));
    }

    #[test]
    fn timer_does_not_panic_when_disabled() {
        // Ensure construction + drop is harmless when env is unset.
        // `enabled()` reads env at emit time, so this is enough as a
        // smoke test of the no-op path.
        let timer = SurfaceStageTimer::start("test_stage");
        timer.finish_ok();
    }
}
