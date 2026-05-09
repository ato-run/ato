//! Unified pre-launch resolution modal (#117).
//!
//! Replaces the previous split-modal experience where a launch would
//! trip an E103 secret modal, retry, then trip an E302 consent modal,
//! retry, then trip another E302 for the next target — four separate
//! modals for one user-perceived launch. The orchestrator drain now
//! merges every E103 / E302 surface into [`PendingResolutionRequest`],
//! and this overlay renders ALL pending requirements (secret rows +
//! consent summaries) in one panel with one Submit button.
//!
//! ## Lazy aggregation
//!
//! The CLI launch loop still surfaces requirements one at a time
//! (E103 first, E302 per target after each retry). When a new
//! requirement arrives while this modal is open, the host merges it
//! into the same [`PendingResolutionRequest`] and we re-render in
//! place — the user sees the panel grow rather than dismiss + re-open.
//! Compared to the prior four-modal sequence this is a "natural"
//! single-modal experience even though the CLI hasn't yet been
//! taught to emit the aggregate envelope upfront (that's tracked
//! separately).
//!
//! ## Submit semantics
//!
//! On Submit, the host:
//! 1. Persists every secret to the user's `SecretStore` and grants
//!    the value to the launch handle so the retry's env carries it.
//! 2. Calls `ato internal consent approve-execution-plan` for every
//!    consent item — same plumbing the legacy
//!    [`crate::ui::modals::consent_form`] already uses, but in a
//!    loop so a multi-target capsule's ExecutionPlans land in one go.
//! 3. Clears [`AppState::pending_resolution`].
//! 4. Re-arms the launch through the existing
//!    `ensure_pending_local_launch` path.
//!
//! ## Cancel semantics
//!
//! Cancel clears `pending_resolution` and marks the active web pane
//! `LaunchFailed` so `ensure_pending_local_launch` does not
//! immediately re-trip the same requirements. The user re-opens the
//! launch from the omnibar.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, AnyElement, BoxShadow, Context, Entity, FontWeight, IntoElement,
    MouseButton, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;

use capsule_wire::config::{ConfigField, ConfigKind};

use crate::app::{
    CancelResolutionForm, ResolutionFormBack, ResolutionFormNext, SubmitResolutionForm,
};
use crate::state::{PendingResolutionRequest, PendingSecretsItem};
use crate::ui::theme::Theme;
use crate::ui::DesktopShell;

/// Two-step navigation for the unified resolution modal.
///
/// Consent review comes first because it's read-only and
/// contextualises why the user is being asked for secrets afterwards.
/// Single-side requests (no consents *or* no secrets) collapse to a
/// single step — the modal still renders, just without Next/Back
/// navigation. The step state is owned by [`ResolutionModal`] and
/// preserved across re-renders so a mid-resolution merge
/// (orchestrator drain merging another envelope into the existing
/// `pending_resolution`) does not snap the user back to the first
/// step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum ResolutionStep {
    Consent,
    Secrets,
}

/// Per-modal view state. One instance lives in `DesktopShell` for as
/// long as `AppState::pending_resolution` is `Some`.
///
/// Holds a separate `InputState` entity per requested secret field so
/// keystrokes/cursor state are preserved across re-renders. Consent
/// items are read-only and need no per-item state — they ride along
/// in the snapshotted `request` for display only.
///
/// Because new requirements can be merged in mid-render, the
/// `should_rebuild_for` check below is order-insensitive across both
/// secrets and consents — we only rebuild when the handle changes
/// (a fresh launch) or a previously-input secret field disappears
/// from the schema (which would orphan focus). New fields appearing
/// or new consents arriving are reconciled in place.
pub(in crate::ui) struct ResolutionModal {
    pub(in crate::ui) request: PendingResolutionRequest,
    pub(in crate::ui) inputs: HashMap<String, Entity<InputState>>,
    /// Currently-active step. Initial value picked by
    /// [`Self::initial_step_for`] so a no-consents request opens
    /// directly on the secrets form (single-step mode); a
    /// no-secrets request opens on consent and the Submit button
    /// approves directly.
    pub(in crate::ui) step: ResolutionStep,
}

impl ResolutionModal {
    pub(in crate::ui) fn new(
        request: PendingResolutionRequest,
        window: &mut Window,
        cx: &mut Context<DesktopShell>,
    ) -> Self {
        let mut inputs = HashMap::new();
        for item in &request.secrets {
            for field in &item.fields {
                let entity = make_input(field, window, cx);
                inputs.insert(input_key(item.target.as_deref(), &field.name), entity);
            }
        }
        let step = Self::initial_step_for(&request);
        Self {
            request,
            inputs,
            step,
        }
    }

    /// Pick the opening step for a freshly-constructed modal. Consent
    /// first when present (the user reviews what they're about to
    /// approve before being asked for secrets), otherwise jump to
    /// secrets so single-step capsules don't show an empty consent
    /// screen.
    fn initial_step_for(request: &PendingResolutionRequest) -> ResolutionStep {
        if request.consents.is_empty() {
            ResolutionStep::Secrets
        } else {
            ResolutionStep::Consent
        }
    }

    /// Advance from consent → secrets. No-op if already on secrets or
    /// if the request has no secrets to advance to (the Submit button
    /// is the right action there).
    pub(in crate::ui) fn advance_step(&mut self) {
        if self.step == ResolutionStep::Consent && !self.request.secrets.is_empty() {
            self.step = ResolutionStep::Secrets;
        }
    }

    /// Go back from secrets → consent. No-op if the request has no
    /// consents (single-step mode).
    pub(in crate::ui) fn retreat_step(&mut self) {
        if self.step == ResolutionStep::Secrets && !self.request.consents.is_empty() {
            self.step = ResolutionStep::Consent;
        }
    }

    /// Reconcile the modal against an updated [`PendingResolutionRequest`].
    /// Returns `true` if a full rebuild is needed (handle changed or a
    /// previously-input field disappeared from the schema). For benign
    /// additions (new consent merged in, new secrets target appended)
    /// the caller can patch in place via [`Self::merge_inputs_for`].
    pub(in crate::ui) fn should_rebuild_for(&self, request: &PendingResolutionRequest) -> bool {
        if self.request.handle != request.handle {
            return true;
        }
        // Secret-field churn: rebuild only if a previously-rendered
        // (target, field) is now missing — orphaned focus is the only
        // case we can't reconcile in place. New keys are fine to add.
        let new_keys: std::collections::HashSet<String> = request
            .secrets
            .iter()
            .flat_map(|item| {
                let target = item.target.clone();
                item.fields
                    .iter()
                    .map(move |field| input_key(target.as_deref(), &field.name))
            })
            .collect();
        for item in &self.request.secrets {
            for field in &item.fields {
                if !new_keys.contains(&input_key(item.target.as_deref(), &field.name)) {
                    return true;
                }
            }
        }
        false
    }

    /// Patch the modal's input map to match a freshly-merged request
    /// without losing existing keystroke state. Adds any new
    /// `(target, field)` keys; replaces the snapshotted request so
    /// the next render iterates the latest secrets + consents lists.
    /// Caller is responsible for ensuring `should_rebuild_for` returned
    /// `false` first.
    pub(in crate::ui) fn merge_inputs_for(
        &mut self,
        request: PendingResolutionRequest,
        window: &mut Window,
        cx: &mut Context<DesktopShell>,
    ) {
        for item in &request.secrets {
            for field in &item.fields {
                let key = input_key(item.target.as_deref(), &field.name);
                if !self.inputs.contains_key(&key) {
                    let entity = make_input(field, window, cx);
                    self.inputs.insert(key, entity);
                }
            }
        }
        // Preserve the user's current step across merges in the
        // common case: if the merge added something the current step
        // can show, stay put. Only re-init when the merge would leave
        // the user staring at an empty step (e.g. consent step with
        // no consents left after a re-emit).
        let step_now_empty = match self.step {
            ResolutionStep::Consent => request.consents.is_empty(),
            ResolutionStep::Secrets => request.secrets.is_empty(),
        };
        if step_now_empty {
            self.step = Self::initial_step_for(&request);
        }
        self.request = request;
    }

    /// Returns the current input value for a `(target, field)` pair, or
    /// the field's default if the input entity is missing (should not
    /// happen in practice, but the host's submit handler treats this
    /// as "do not write" rather than panicking).
    pub(in crate::ui) fn read_input(
        &self,
        target: Option<&str>,
        field_name: &str,
        cx: &Context<DesktopShell>,
    ) -> Option<String> {
        let key = input_key(target, field_name);
        let entity = self.inputs.get(&key)?;
        let value = entity.read(cx).value();
        Some(value.to_string())
    }
}

fn input_key(target: Option<&str>, field_name: &str) -> String {
    match target {
        Some(t) => format!("{t}::{field_name}"),
        None => format!("__top__::{field_name}"),
    }
}

fn make_input(
    field: &ConfigField,
    window: &mut Window,
    cx: &mut Context<DesktopShell>,
) -> Entity<InputState> {
    let placeholder = field.placeholder.clone().unwrap_or_default();
    let default = field.default.clone().unwrap_or_default();
    let masked = matches!(field.kind, ConfigKind::Secret);
    cx.new(|cx| {
        let mut state = InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(default);
        if masked {
            state = state.masked(true);
        }
        state
    })
}

/// Stateless overlay renderer. The parent (`DesktopShell`) owns the
/// `ResolutionModal` instance and feeds it in alongside the active
/// `Theme`; this function is pure projection.
///
/// The body renders only the current step's content (consent
/// summaries OR secret input rows) and wraps it in a vertical
/// scroll container so multi-target capsules with long policy
/// summaries don't clip against the modal's `max_h` cap. The button
/// row depends on the step:
///
/// - **Consent step, secrets pending**: `[Cancel]` `[Continue]`
/// - **Consent step, no secrets**: `[Cancel]` `[Approve & Launch]`
/// - **Secrets step, consents present**: `[← Back]` `[Approve & Launch]`
/// - **Secrets step, no consents**: `[Cancel]` `[Approve & Launch]`
pub(in crate::ui) fn render_resolution_modal_overlay(
    modal: &ResolutionModal,
    theme: &Theme,
) -> AnyElement {
    let request = &modal.request;
    let secret_count: usize = request.secrets.iter().map(|s| s.fields.len()).sum();
    let consent_count = request.consents.len();
    let header_summary = format_summary_line(secret_count, consent_count);
    let step_indicator = render_step_indicator(modal, theme);
    let body = render_step_body(modal, theme);
    let buttons = render_step_buttons(modal, theme);

    div()
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.42))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(620.0))
                .max_w(px(760.0))
                .max_h(px(720.0))
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
                        .child(format!("Approve and launch {}", request.handle)),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_secondary)
                        .child(header_summary),
                )
                .child(step_indicator)
                // Scrollable body — vertical overflow is the common
                // case (long ExecutionPlan summaries on multi-target
                // capsules). `flex_grow + min_h(0) + overflow_y_scrollbar`
                // is the GPUI idiom for "let me grow into available
                // space and scroll if I exceed it" inside a
                // flex_col parent. `overflow_y_scrollbar` is the
                // method gpui-component exposes — it sets vertical
                // scroll behaviour and renders a scrollbar.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .flex_grow()
                        .min_h(px(0.0))
                        .overflow_y_scrollbar()
                        .child(body),
                )
                .child(buttons),
        )
        .into_any_element()
}

/// Render a horizontal stepper showing both steps as numbered
/// breadcrumbs so the user can see at a glance "I'm on step 1 of 2,
/// step 2 is still ahead". Single-step requests collapse to nothing —
/// drawing a stepper for one step is more confusing than helpful.
///
/// Visual:
///
/// ```text
///  ┌───┐                          ┌───┐
///  │ 1 │ Review consents     →    │ 2 │ Provide secrets
///  └───┘ (highlighted)            └───┘ (dim)
/// ```
///
/// Active step uses `accent_subtle` background + `text_primary` text;
/// inactive step uses transparent background + `text_tertiary` text.
/// The arrow between is the visual cue that step 2 is the next
/// destination.
fn render_step_indicator(modal: &ResolutionModal, theme: &Theme) -> AnyElement {
    let consents_present = !modal.request.consents.is_empty();
    let secrets_present = !modal.request.secrets.is_empty();
    if !consents_present || !secrets_present {
        // Single-step mode — no indicator clutter; the header
        // already says what's happening.
        return div().into_any_element();
    }

    let on_consent = modal.step == ResolutionStep::Consent;

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(render_step_pill(
            1,
            "Review consents",
            on_consent,
            theme,
        ))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_tertiary)
                .child("→"),
        )
        .child(render_step_pill(
            2,
            "Provide secrets",
            !on_consent,
            theme,
        ))
        .into_any_element()
}

/// One numbered pill in the stepper. The number is rendered in a
/// rounded box so the user reads "1" / "2" before the label — a
/// stronger affordance than the prior "Step 1 of 2" text.
fn render_step_pill(
    number: u8,
    label: &'static str,
    active: bool,
    theme: &Theme,
) -> impl IntoElement {
    let (badge_bg, badge_border, badge_text, label_text) = if active {
        (
            theme.accent_subtle,
            theme.accent_border,
            theme.text_primary,
            theme.text_primary,
        )
    } else {
        (
            theme.panel_bg,
            theme.border_default,
            theme.text_tertiary,
            theme.text_tertiary,
        )
    };

    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(22.0))
                .h(px(22.0))
                .rounded(px(11.0))
                .border_1()
                .border_color(badge_border)
                .bg(badge_bg)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.0))
                .font_weight(FontWeight(700.0))
                .text_color(badge_text)
                .child(format!("{number}")),
        )
        .child(
            div()
                .text_size(px(11.5))
                .font_weight(if active {
                    FontWeight(600.0)
                } else {
                    FontWeight(500.0)
                })
                .text_color(label_text)
                .child(label),
        )
}

fn render_step_body(modal: &ResolutionModal, theme: &Theme) -> AnyElement {
    match modal.step {
        ResolutionStep::Consent => {
            if modal.request.consents.is_empty() {
                // Edge: step is Consent but the list is empty (a
                // merge cleared every consent between renders). The
                // step transitions on next reconcile; render a brief
                // placeholder rather than a confusing empty box.
                return placeholder_text(
                    "No ExecutionPlans pending review. Continue to provide secrets.",
                    theme,
                );
            }
            let mut children: Vec<AnyElement> = Vec::new();
            for item in &modal.request.consents {
                children.push(render_consent_section(item, theme).into_any_element());
            }
            div()
                .flex()
                .flex_col()
                .gap_3()
                .children(children)
                .into_any_element()
        }
        ResolutionStep::Secrets => {
            if modal.request.secrets.is_empty() {
                return placeholder_text(
                    "No secrets pending. Approve to launch.",
                    theme,
                );
            }
            let mut children: Vec<AnyElement> = Vec::new();
            for item in &modal.request.secrets {
                children.push(render_secrets_section(modal, item, theme).into_any_element());
            }
            div()
                .flex()
                .flex_col()
                .gap_3()
                .children(children)
                .into_any_element()
        }
    }
}

fn placeholder_text(message: &'static str, theme: &Theme) -> AnyElement {
    div()
        .text_size(px(12.0))
        .text_color(theme.text_secondary)
        .child(message)
        .into_any_element()
}

fn render_step_buttons(modal: &ResolutionModal, theme: &Theme) -> AnyElement {
    let consents_present = !modal.request.consents.is_empty();
    let secrets_present = !modal.request.secrets.is_empty();
    let on_secrets_step = modal.step == ResolutionStep::Secrets;
    let on_consent_step_with_secrets = modal.step == ResolutionStep::Consent && secrets_present;

    let mut row = div().mt_2().flex().gap_2().justify_end();

    // Left button: Back if we can go back, otherwise Cancel.
    if on_secrets_step && consents_present {
        row = row.child(render_modal_button(
            "← Back",
            theme.panel_bg,
            theme.border_default,
            theme.text_secondary,
            ResolutionFormBack,
        ));
    } else {
        row = row.child(render_modal_button(
            "Cancel",
            theme.panel_bg,
            theme.border_default,
            theme.text_secondary,
            CancelResolutionForm,
        ));
    }

    // Right button: Continue if there's a next step, otherwise Submit.
    if on_consent_step_with_secrets {
        row = row.child(render_modal_button(
            "Continue →",
            theme.accent_subtle,
            theme.accent_border,
            theme.text_primary,
            ResolutionFormNext,
        ));
    } else {
        row = row.child(render_modal_button(
            "Approve & Launch",
            theme.accent_subtle,
            theme.accent_border,
            theme.text_primary,
            SubmitResolutionForm,
        ));
    }

    row.into_any_element()
}

fn format_summary_line(secret_count: usize, consent_count: usize) -> String {
    let secrets_part = match secret_count {
        0 => None,
        1 => Some("1 secret".to_string()),
        n => Some(format!("{n} secrets")),
    };
    let consents_part = match consent_count {
        0 => None,
        1 => Some("1 ExecutionPlan to approve".to_string()),
        n => Some(format!("{n} ExecutionPlans to approve")),
    };
    match (secrets_part, consents_part) {
        (Some(a), Some(b)) => format!("Provide {a} and {b}."),
        (Some(a), None) => format!("Provide {a}."),
        (None, Some(b)) => format!("There is {b}."),
        // Shouldn't happen in practice — the host clears
        // pending_resolution when both lists are empty.
        (None, None) => "Nothing to resolve.".to_string(),
    }
}

fn render_section_header(label: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .text_size(px(11.5))
        .font_weight(FontWeight(600.0))
        .text_color(theme.text_tertiary)
        .child(label)
}

fn render_secrets_section(
    modal: &ResolutionModal,
    item: &PendingSecretsItem,
    theme: &Theme,
) -> impl IntoElement {
    let target_label = item
        .target
        .as_deref()
        .map(|t| format!("target {t}"))
        .unwrap_or_else(|| "top-level".to_string());

    let rows: Vec<AnyElement> = item
        .fields
        .iter()
        .map(|field| {
            let input = modal
                .inputs
                .get(&input_key(item.target.as_deref(), &field.name));
            render_field_row(field, input, theme).into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .border_1()
        .border_color(theme.border_default)
        .rounded(px(10.0))
        .p_3()
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.text_secondary)
                .child(target_label),
        )
        .child(div().flex().flex_col().gap_3().children(rows))
}

fn render_consent_section(
    item: &crate::state::PendingConsentItem,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .border_1()
        .border_color(theme.border_default)
        .rounded(px(10.0))
        .p_3()
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.text_secondary)
                .child(format!("target {}", item.target_label)),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.text_primary)
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.border_default)
                .rounded(px(8.0))
                .p_2()
                .font_family("monospace")
                .whitespace_normal()
                .child(item.summary.clone()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(10.5))
                .text_color(theme.text_tertiary)
                .child(format!("policy_segment_hash: {}", item.policy_segment_hash))
                .child(format!(
                    "provisioning_policy_hash: {}",
                    item.provisioning_policy_hash
                )),
        )
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

    let style_input = |i: Input| {
        i.h(px(32.0))
            .text_size(px(13.0))
            .text_color(theme.text_primary)
            .bg(theme.settings_body_bg)
    };
    let input_box = match input {
        Some(entity) => match &field.kind {
            ConfigKind::Enum { choices } => {
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
