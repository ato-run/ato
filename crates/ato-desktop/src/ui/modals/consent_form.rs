//! ExecutionPlan consent modal (E302).
//!
//! Triggered when `start_capsule` aborts with E302 carrying
//! `details.reason = "execution_plan_consent_required"`. The
//! orchestrator parses the envelope, builds a
//! [`PendingConsentRequest`], and drops it onto
//! [`AppState::pending_consent`]. `DesktopShell` observes the
//! `None → Some` transition, allocates a [`ConsentModal`] (a snapshot
//! of the request), and renders this overlay on top of the stage.
//!
//! ## Approve / Cancel semantics
//!
//! - **Approve** dispatches [`ApproveConsentForm`]; the host calls
//!   `ato internal consent approve-execution-plan` (the CLI owns the
//!   `executionplan_v1.jsonl` write — desktop never touches it
//!   directly), records the per-handle retry-once budget, and clears
//!   `pending_consent`. `ensure_pending_local_launch` re-arms the
//!   launch on the next render.
//! - **Cancel** dispatches [`CancelConsentForm`]; the host clears
//!   `pending_consent` and marks the active web pane as
//!   `LaunchFailed` so `ensure_pending_local_launch` does NOT
//!   immediately re-trip the same E302. The user reopens the launch
//!   by re-entering the handle in the omnibar.
//!
//! ## Why no input fields
//!
//! Unlike the E103 config modal, every value the modal needs to
//! display AND every value the Approve handler needs to send to
//! `approve-execution-plan` is already in `PendingConsentRequest` —
//! the CLI envelope carries the full identity tuple
//! (`scoped_id`, `version`, `target_label`, `policy_segment_hash`,
//! `provisioning_policy_hash`) plus a pre-rendered summary. The modal
//! is read-only by design; the user's only inputs are Approve / Cancel.

use gpui::prelude::*;
use gpui::{div, hsla, point, px, AnyElement, BoxShadow, FontWeight, IntoElement, MouseButton};

use crate::app::{ApproveConsentForm, CancelConsentForm};
use crate::state::PendingConsentRequest;
use crate::ui::theme::Theme;

/// Snapshot of [`PendingConsentRequest`] held for the lifetime of the
/// modal overlay. The host (`DesktopShell`) rebuilds the snapshot if
/// the underlying request changes (different handle / different
/// digests) rather than mutating in place — every field is part of
/// the consent identity and partial mutation would silently corrupt
/// the round-trip back to `approve-execution-plan`.
pub(in crate::ui) struct ConsentModal {
    pub(in crate::ui) request: PendingConsentRequest,
}

impl ConsentModal {
    pub(in crate::ui) fn new(request: PendingConsentRequest) -> Self {
        Self { request }
    }

    /// Cheap structural check: returns true if the displayed snapshot
    /// is stale w.r.t. the live `pending_consent`. Identity-tuple
    /// match — any drift means we rebuild.
    pub(in crate::ui) fn needs_rebuild(&self, latest: &PendingConsentRequest) -> bool {
        self.request.handle != latest.handle
            || self.request.scoped_id != latest.scoped_id
            || self.request.version != latest.version
            || self.request.target_label != latest.target_label
            || self.request.policy_segment_hash != latest.policy_segment_hash
            || self.request.provisioning_policy_hash != latest.provisioning_policy_hash
    }
}

/// Stateless overlay renderer. The parent (`DesktopShell`) owns the
/// `ConsentModal` instance and feeds it in alongside the active
/// `Theme`; this function is pure projection.
pub(in crate::ui) fn render_consent_modal_overlay(
    modal: &ConsentModal,
    theme: &Theme,
) -> AnyElement {
    let request = &modal.request;
    let header_subtitle = format!("{} · target {}", request.handle, request.target_label);

    div()
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.42))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(560.0))
                .max_w(px(720.0))
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
                        .child("Approve ExecutionPlan to launch this capsule"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_secondary)
                        .child(header_subtitle),
                )
                .child(
                    // Pre-rendered plan summary from the CLI envelope.
                    // Whitespace-significant: monospace + preserve
                    // newlines so the policy hashes / network rules
                    // line up the way `consent_summary(plan)` produced
                    // them.
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_primary)
                        .bg(theme.settings_body_bg)
                        .border_1()
                        .border_color(theme.border_default)
                        .rounded(px(10.0))
                        .p_3()
                        .font_family("monospace")
                        .whitespace_normal()
                        .child(request.summary.clone()),
                )
                .child(
                    // Hashes the user is consenting to. Surfacing
                    // these in the modal (not just the summary) lets
                    // a careful operator cross-reference against the
                    // capsule's published manifest before approving.
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(format!(
                            "policy_segment_hash: {}",
                            request.policy_segment_hash
                        ))
                        .child(format!(
                            "provisioning_policy_hash: {}",
                            request.provisioning_policy_hash
                        )),
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
                            CancelConsentForm,
                        ))
                        .child(render_modal_button(
                            "Approve & Launch",
                            theme.accent_subtle,
                            theme.accent_border,
                            theme.text_primary,
                            ApproveConsentForm,
                        )),
                ),
        )
        .into_any_element()
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
