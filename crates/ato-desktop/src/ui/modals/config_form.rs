//! Schema-driven config form modal (Feature 2 — Day 4).
//!
//! Triggered when `start_capsule` aborts with E103 — the orchestrator
//! parses the CLI's missing-env envelope, builds a
//! [`PendingConfigRequest`], and drops it onto
//! [`AppState::pending_config`]. `DesktopShell` observes the
//! transition `None → Some`, allocates a [`ConfigModal`], and renders
//! its overlay on top of the stage. On Save the host writes each
//! field back into the secret store and grants it to the capsule;
//! once `pending_config` is cleared, `ensure_pending_local_launch`
//! re-arms the same handle and the run proceeds with the freshly
//! supplied secrets in the env.
//!
//! ## Why this lives in `ato-desktop`, not a shared UI crate
//!
//! The modal binds against `gpui_component::input::InputState`, owns
//! `Entity<InputState>` per field, and is dispatched via the
//! desktop's `actions!()` registry (`SaveConfigForm` /
//! `CancelConfigForm`). All of that is desktop-shell-private — we
//! consume the canonical wire types (`capsule_wire::config::ConfigField`
//! / `ConfigKind`) directly here, but keep all GPUI rendering in this
//! module so a future UI rewrite doesn't ripple into the wire contract.
//!
//! ## Day-4 MVP scope
//!
//! Only `ConfigKind::Secret` rows are wired through Save: the value
//! is added to the global `SecretStore` and granted to the capsule
//! handle so `secrets_for_capsule(handle)` returns it on retry.
//! Non-secret kinds (`String`, `Number`, `Enum`) still render — the
//! form looks complete to the user — but their values are *not*
//! persisted yet (Day 5). This keeps the surface honest: Day 4 ships
//! the BYOK secret flow end-to-end, nothing more.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, AnyElement, BoxShadow, Context, Entity, FontWeight, IntoElement,
    MouseButton, Window,
};
use gpui_component::input::{Input, InputState};

use capsule_wire::config::{ConfigField, ConfigKind};

use crate::app::{CancelConfigForm, SaveConfigForm};
use crate::state::PendingConfigRequest;
use crate::ui::theme::Theme;
use crate::ui::DesktopShell;

/// Per-modal view state. One instance lives in `DesktopShell` for as
/// long as `AppState::pending_config` is `Some`.
///
/// Holds a separate `InputState` entity per requested field so
/// keystrokes/cursor state are preserved across re-renders. The
/// `request` field is a snapshot taken at construction time — if the
/// pending request changes (different handle, different fields)
/// `DesktopShell` rebuilds the entire modal rather than mutating in
/// place, since each field's input is keyed by name and adding/
/// removing keys mid-flight would orphan focus.
pub(in crate::ui) struct ConfigModal {
    /// Snapshot of the launch failure that triggered this modal. The
    /// `handle` and `original_secrets` are needed at Save time to
    /// grant the new secrets to the right capsule.
    pub(in crate::ui) request: PendingConfigRequest,
    /// One `InputState` entity per `ConfigField.name`. Indexed by
    /// the schema-supplied env-var name; never index-aligned with
    /// `request.fields` so a future schema reorder can't desync.
    pub(in crate::ui) inputs: HashMap<String, Entity<InputState>>,
}

impl ConfigModal {
    /// Construct the modal and seed each field's `InputState`.
    ///
    /// `Secret` rows are masked at the input layer
    /// (`InputState::masked(true)`) so the bullet rendering is the
    /// component's responsibility — the host never sees a plaintext
    /// flash even mid-keystroke. Defaults from the schema are seeded
    /// via `default_value` so the user sees the suggestion and can
    /// accept it by hitting Save without typing.
    pub(in crate::ui) fn new(
        request: PendingConfigRequest,
        window: &mut Window,
        cx: &mut Context<DesktopShell>,
    ) -> Self {
        let mut inputs = HashMap::with_capacity(request.fields.len());
        for field in &request.fields {
            let placeholder = field.placeholder.clone().unwrap_or_default();
            let default = field.default.clone().unwrap_or_default();
            let masked = matches!(field.kind, ConfigKind::Secret);
            let entity = cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(default);
                if masked {
                    state = state.masked(true);
                }
                state
            });
            inputs.insert(field.name.clone(), entity);
        }
        Self { request, inputs }
    }

    /// Returns true when a fresh `request` should rebuild this modal
    /// from scratch instead of being patched in. We rebuild on:
    ///   * different capsule handle (a new failed launch)
    ///   * different field set (schema changed mid-session — rare,
    ///     but cheaper to rebuild than to reconcile)
    pub(in crate::ui) fn should_rebuild_for(&self, request: &PendingConfigRequest) -> bool {
        if self.request.handle != request.handle {
            return true;
        }
        if self.request.fields.len() != request.fields.len() {
            return true;
        }
        // Order-insensitive name check — the schema is authoritative
        // by `name`, so a reordering alone shouldn't toss user input.
        let new_names: std::collections::HashSet<&str> =
            request.fields.iter().map(|f| f.name.as_str()).collect();
        for field in &self.request.fields {
            if !new_names.contains(field.name.as_str()) {
                return true;
            }
        }
        false
    }
}

/// Stateless overlay renderer. The parent (`DesktopShell`) owns the
/// `ConfigModal` instance and feeds it in alongside the active
/// `Theme`; this function is pure projection.
pub(in crate::ui) fn render_config_modal_overlay(modal: &ConfigModal, theme: &Theme) -> AnyElement {
    let request = &modal.request;
    let target_label = request
        .target
        .as_deref()
        .map(|t| format!(" · target {t}"))
        .unwrap_or_default();
    let header_subtitle = format!("{}{}", request.handle, target_label);

    let row_count = request.fields.len();
    let rows: Vec<AnyElement> = request
        .fields
        .iter()
        .map(|field| {
            let input = modal.inputs.get(&field.name);
            render_field_row(field, input, theme).into_any_element()
        })
        .collect();

    div()
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.42))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(520.0))
                .max_w(px(640.0))
                .max_h(px(640.0))
                .rounded(px(18.0))
                .bg(theme.panel_bg)
                .border_1()
                .border_color(theme.accent_border)
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.22),
                    offset: point(px(0.0), px(18.0)),
                    blur_radius: px(48.0),
                    spread_radius: px(0.0),
                }])
                .p_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("This capsule needs configuration"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_secondary)
                        .child(format!(
                            "Provide {} value{} to launch {}.",
                            row_count,
                            if row_count == 1 { "" } else { "s" },
                            header_subtitle,
                        )),
                )
                .child(
                    // Field rows. Day 4 MVP: no scroll container —
                    // the outer panel's `max_h(640px)` clips and the
                    // typical schema is 1-3 fields. Day 7 wires up
                    // `gpui_component::scroll::ScrollableElement` if
                    // real-world capsules push past the cap.
                    div().flex().flex_col().gap_3().children(rows),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(render_modal_button(
                            "Cancel",
                            theme.panel_bg,
                            theme.border_default,
                            theme.text_secondary,
                            CancelConfigForm,
                        ))
                        .child(render_modal_button(
                            "Save & Launch",
                            theme.accent_subtle,
                            theme.accent_border,
                            theme.text_primary,
                            SaveConfigForm,
                        )),
                ),
        )
        .into_any_element()
}

fn render_field_row(
    field: &ConfigField,
    input: Option<&Entity<InputState>>,
    theme: &Theme,
) -> impl IntoElement {
    let label = field.label.clone().unwrap_or_else(|| field.name.clone());
    let kind_hint = match &field.kind {
        ConfigKind::Secret => "secret · stored locally, masked",
        ConfigKind::String => "text",
        ConfigKind::Number => "number",
        ConfigKind::Enum { .. } => "choice",
    };

    let mut row = div().flex().flex_col().gap_1().child(
        div()
            .flex()
            .items_baseline()
            .justify_between()
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight(600.0))
                    .text_color(theme.text_primary)
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_tertiary)
                    .child(kind_hint),
            ),
    );

    if let Some(description) = &field.description {
        if !description.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_secondary)
                    .child(description.clone()),
            );
        }
    }

    // The Input widget inherits its text color from `gpui_component`'s
    // independent theme global, not from our `Theme`. In dark/auto
    // appearance the inherited foreground can land near-white on our
    // light panel background — making the value invisible. Pin colors
    // to our theme so the field is always legible regardless of how
    // gpui_component's appearance is currently configured.
    let style_input = |i: Input| {
        i.h(px(32.0))
            .text_size(px(13.0))
            .text_color(theme.text_primary)
            .bg(theme.settings_body_bg)
    };
    let input_box = match input {
        Some(entity) => match &field.kind {
            ConfigKind::Enum { choices } => {
                // MVP: render the dropdown's choices inline as a hint
                // and fall back to a plain text input. The native
                // dropdown component is wired up in Day 5 — keeping
                // the surface honest about what's actually validated
                // beats a half-working selector.
                let hint = format!("choices: {}", choices.join(", "));
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(hint),
                    )
                    .child(style_input(Input::new(entity)))
                    .into_any_element()
            }
            _ => style_input(Input::new(entity)).into_any_element(),
        },
        None => div()
            .h(px(32.0))
            .flex()
            .items_center()
            .px_2()
            .text_size(px(11.5))
            .text_color(theme.text_tertiary)
            .child("(input unavailable)")
            .into_any_element(),
    };

    row.child(input_box)
}

fn render_modal_button<A: gpui::Action + Clone + 'static>(
    label: &'static str,
    bg: gpui::Hsla,
    border: gpui::Hsla,
    text: gpui::Hsla,
    action: A,
) -> impl IntoElement {
    div()
        .rounded(px(10.0))
        .px_3()
        .py_2()
        .border_1()
        .border_color(border)
        .bg(bg)
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(text)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.dispatch_action(Box::new(action.clone()), cx);
        })
        .child(label)
}
