use gpui::Hsla;

use crate::state::ThemeMode;

/// Derive a stable hue (0..360) for a task icon from its identity.
///
/// The icon color must be a function of the task itself, not the
/// task's position in the rail or its active/inactive state — the
/// same task should look the same wherever it is rendered (sidebar
/// chip, drag preview, future overview cards) and across its
/// lifecycle. We use the golden-ratio conjugate as a hue increment
/// so adjacent IDs land far apart on the color wheel without a
/// precomputed palette table.
pub fn task_hue(seed: u64) -> f32 {
    const GOLDEN_CONJUGATE: f32 = 0.618_034;
    // f32 is enough — we only need stable bucketing into 360°. Cast via u32 to
    // keep the magnitude small enough for fract() to behave on huge IDs.
    let truncated = (seed & 0xFFFF_FFFF) as f32;
    (truncated * GOLDEN_CONJUGATE).fract() * 360.0
}

pub struct Theme {
    pub mode: ThemeMode,

    // Shell root
    pub canvas_bg: Hsla,
    pub canvas_text: Hsla,
    pub ambient_glow_top: Hsla,

    // Chrome + sidebar rail
    pub panel_bg: Hsla,
    pub panel_border: Hsla,

    // Stage container
    pub stage_bg: Hsla,
    pub stage_border: Hsla,
    pub stage_shadow_far: Hsla,
    pub stage_shadow_near: Hsla,

    // Pane gradient stops
    pub pane_bg_top: Hsla,
    pub pane_bg_bottom: Hsla,

    // Settings panel
    pub settings_panel_bg: Hsla,
    pub settings_card_bg: Hsla,
    pub settings_card_border: Hsla,
    pub settings_body_bg: Hsla,
    pub settings_body_border: Hsla,

    // Text
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_disabled: Hsla,

    // Borders
    pub border_subtle: Hsla,
    pub border_default: Hsla,
    pub border_strong: Hsla,

    // Interactive surfaces
    pub surface_hover: Hsla,
    pub surface_pressed: Hsla,

    // Accent
    pub accent: Hsla,
    pub accent_subtle: Hsla,
    pub accent_border: Hsla,

    // Overview overlay
    pub overlay_bg: Hsla,
    pub overlay_header_text: Hsla,
    pub overview_card_bg_active: Hsla,
    pub overview_card_bg_inactive: Hsla,
    pub overview_card_border_inactive: Hsla,

    // Omnibar
    pub omnibar_rest_bg: Hsla,
    pub omnibar_rest_border: Hsla,
    pub omnibar_active_bg: Hsla,
    pub omnibar_active_border: Hsla,
    pub omnibar_text: Hsla,
    pub omnibar_placeholder: Hsla,
    pub omnibar_icon_rest: Hsla,
    pub omnibar_icon_active: Hsla,
    pub omnibar_dropdown_bg: Hsla,
    pub omnibar_dropdown_border: Hsla,
    pub omnibar_suggestion_title: Hsla,
    pub omnibar_suggestion_detail: Hsla,
    pub omnibar_suggestion_hover: Hsla,

    // Preview cards (design_system.rs)
    pub preview_card_bg: Hsla,
    pub preview_chrome_bg: Hsla,

    // Sidebar rail (left vertical strip). Separate from
    // panel_bg / panel_border because the mockup at
    // .tmp/sidebar.html uses an `elevated: #111113` surface that
    // sits between the deeper chrome `#09090b` and the lighter
    // floating panels.
    pub sidebar_bg: Hsla,
    pub sidebar_border: Hsla,
}

impl Theme {
    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }

    pub fn light() -> Self {
        use gpui::hsla;
        Self {
            mode: ThemeMode::Light,

            canvas_bg: hsla(60.0 / 360.0, 0.03, 0.965, 1.0), // #F7F7F5
            canvas_text: hsla(0.0, 0.0, 0.090, 1.0),         // #171717
            ambient_glow_top: hsla(0.0, 0.0, 0.0, 0.0),      // none

            panel_bg: hsla(0.0, 0.0, 1.0, 0.92),
            panel_border: hsla(60.0 / 360.0, 0.05, 0.897, 1.0), // #E6E6E1

            stage_bg: hsla(60.0 / 360.0, 0.05, 0.950, 1.0), // #F3F3F1
            stage_border: hsla(60.0 / 360.0, 0.05, 0.847, 1.0), // #D8D8D2
            stage_shadow_far: hsla(0.0, 0.0, 0.0, 0.08),
            stage_shadow_near: hsla(0.0, 0.0, 0.0, 0.04),

            pane_bg_top: hsla(0.0, 0.0, 1.0, 1.0),
            pane_bg_bottom: hsla(60.0 / 360.0, 0.03, 0.965, 1.0),

            settings_panel_bg: hsla(0.0, 0.0, 1.0, 1.0),
            settings_card_bg: hsla(60.0 / 360.0, 0.05, 0.950, 1.0),
            settings_card_border: hsla(60.0 / 360.0, 0.05, 0.847, 1.0),
            settings_body_bg: hsla(60.0 / 360.0, 0.06, 0.933, 1.0), // #EFEFEC
            settings_body_border: hsla(60.0 / 360.0, 0.05, 0.897, 1.0),

            text_primary: hsla(0.0, 0.0, 0.090, 1.0), // #171717
            text_secondary: hsla(0.0, 0.0, 0.333, 1.0), // #555555
            text_tertiary: hsla(0.0, 0.0, 0.478, 1.0), // #7A7A7A
            text_disabled: hsla(0.0, 0.0, 0.638, 1.0), // #A3A3A3

            border_subtle: hsla(60.0 / 360.0, 0.05, 0.897, 1.0), // #E6E6E1
            border_default: hsla(60.0 / 360.0, 0.05, 0.847, 1.0), // #D8D8D2
            border_strong: hsla(60.0 / 360.0, 0.08, 0.784, 1.0), // #C8C8C0

            surface_hover: hsla(60.0 / 360.0, 0.06, 0.933, 1.0), // #EFEFEC
            surface_pressed: hsla(60.0 / 360.0, 0.08, 0.905, 1.0), // #E7E7E3

            accent: hsla(217.0 / 360.0, 0.75, 0.45, 1.0),
            accent_subtle: hsla(217.0 / 360.0, 0.75, 0.45, 0.10),
            accent_border: hsla(217.0 / 360.0, 0.50, 0.55, 0.35),

            overlay_bg: hsla(0.0, 0.0, 0.0, 0.40),
            overlay_header_text: hsla(0.0, 0.0, 0.333, 1.0),
            overview_card_bg_active: hsla(217.0 / 360.0, 0.75, 0.45, 0.08),
            overview_card_bg_inactive: hsla(0.0, 0.0, 1.0, 0.96),
            overview_card_border_inactive: hsla(60.0 / 360.0, 0.05, 0.847, 1.0),

            omnibar_rest_bg: hsla(60.0 / 360.0, 0.06, 0.933, 1.0),
            omnibar_rest_border: hsla(60.0 / 360.0, 0.05, 0.847, 1.0),
            omnibar_active_bg: hsla(0.0, 0.0, 1.0, 1.0),
            omnibar_active_border: hsla(217.0 / 360.0, 0.50, 0.55, 0.35),
            omnibar_text: hsla(0.0, 0.0, 0.090, 1.0),
            omnibar_placeholder: hsla(0.0, 0.0, 0.478, 1.0),
            omnibar_icon_rest: hsla(0.0, 0.0, 0.478, 1.0),
            omnibar_icon_active: hsla(217.0 / 360.0, 0.75, 0.45, 1.0),
            omnibar_dropdown_bg: hsla(0.0, 0.0, 1.0, 0.98),
            omnibar_dropdown_border: hsla(60.0 / 360.0, 0.05, 0.847, 1.0),
            omnibar_suggestion_title: hsla(0.0, 0.0, 0.090, 1.0),
            omnibar_suggestion_detail: hsla(0.0, 0.0, 0.478, 1.0),
            omnibar_suggestion_hover: hsla(60.0 / 360.0, 0.06, 0.933, 1.0),

            preview_card_bg: hsla(60.0 / 360.0, 0.05, 0.950, 1.0),
            preview_chrome_bg: hsla(60.0 / 360.0, 0.06, 0.933, 1.0),

            // Light mode keeps the previous panel surface for the
            // sidebar so the white-mode UI stays familiar; the
            // mockup's #111113 / #27272a are dark-mode specific.
            sidebar_bg: hsla(0.0, 0.0, 1.0, 0.92), // matches panel_bg
            sidebar_border: hsla(60.0 / 360.0, 0.05, 0.897, 1.0), // matches panel_border
        }
    }

    pub fn dark() -> Self {
        use gpui::hsla;
        Self {
            mode: ThemeMode::Dark,

            canvas_bg: hsla(240.0 / 360.0, 0.09, 0.11, 1.0), // #1a1a1e
            canvas_text: hsla(0.0, 0.0, 0.941, 1.0),         // #f0f0f2
            ambient_glow_top: hsla(220.0 / 360.0, 0.30, 0.20, 0.20),

            panel_bg: hsla(240.0 / 360.0, 0.09, 0.13, 0.85),
            panel_border: hsla(0.0, 0.0, 1.0, 0.06),

            stage_bg: hsla(228.0 / 360.0, 0.16, 0.13, 1.0),
            stage_border: hsla(0.0, 0.0, 1.0, 0.06),
            stage_shadow_far: hsla(0.0, 0.0, 0.0, 0.45),
            stage_shadow_near: hsla(0.0, 0.0, 0.0, 0.30),

            pane_bg_top: hsla(240.0 / 360.0, 0.09, 0.114, 1.0), // #1d1d23
            pane_bg_bottom: hsla(240.0 / 360.0, 0.09, 0.098, 1.0), // #19191f

            settings_panel_bg: hsla(240.0 / 360.0, 0.09, 0.122, 1.0), // #1f1f25
            settings_card_bg: hsla(0.0, 0.0, 1.0, 0.03),
            settings_card_border: hsla(0.0, 0.0, 1.0, 0.06),
            settings_body_bg: hsla(0.0, 0.0, 0.0, 0.18),
            settings_body_border: hsla(0.0, 0.0, 1.0, 0.05),

            text_primary: hsla(0.0, 0.0, 0.902, 1.0), // #e6e8ec
            text_secondary: hsla(0.0, 0.0, 0.784, 1.0), // #c6cbd2
            text_tertiary: hsla(0.0, 0.0, 1.0, 0.55),
            text_disabled: hsla(0.0, 0.0, 0.553, 1.0), // #8d929c

            border_subtle: hsla(0.0, 0.0, 1.0, 0.06),
            border_default: hsla(0.0, 0.0, 1.0, 0.10),
            border_strong: hsla(0.0, 0.0, 1.0, 0.15),

            surface_hover: hsla(217.0 / 360.0, 0.60, 0.50, 0.15),
            surface_pressed: hsla(217.0 / 360.0, 0.60, 0.50, 0.22),

            accent: hsla(217.0 / 360.0, 0.91, 0.60, 1.0), // #3b82f6
            accent_subtle: hsla(217.0 / 360.0, 0.60, 0.50, 0.15),
            accent_border: hsla(217.0 / 360.0, 0.88, 0.61, 0.44),

            overlay_bg: hsla(0.0, 0.0, 0.0, 0.60),
            overlay_header_text: hsla(0.0, 0.0, 1.0, 0.55),
            overview_card_bg_active: hsla(225.0 / 360.0, 0.18, 0.18, 0.96),
            overview_card_bg_inactive: hsla(240.0 / 360.0, 0.06, 0.16, 0.96),
            overview_card_border_inactive: hsla(0.0, 0.0, 1.0, 0.06),

            omnibar_rest_bg: hsla(0.0, 0.0, 1.0, 0.05),
            omnibar_rest_border: hsla(0.0, 0.0, 1.0, 0.06),
            omnibar_active_bg: hsla(221.0 / 360.0, 0.18, 0.20, 0.98),
            omnibar_active_border: hsla(217.0 / 360.0, 0.88, 0.61, 0.44),
            omnibar_text: hsla(0.0, 0.0, 1.0, 1.0),
            omnibar_placeholder: hsla(0.0, 0.0, 0.776, 1.0), // #c6cbd2
            omnibar_icon_rest: hsla(0.0, 0.0, 0.776, 1.0),
            omnibar_icon_active: hsla(217.0 / 360.0, 0.88, 0.60, 1.0),
            omnibar_dropdown_bg: hsla(224.0 / 360.0, 0.14, 0.12, 0.98),
            omnibar_dropdown_border: hsla(0.0, 0.0, 1.0, 0.08),
            omnibar_suggestion_title: hsla(0.0, 0.0, 0.902, 1.0),
            omnibar_suggestion_detail: hsla(0.0, 0.0, 0.553, 1.0),
            omnibar_suggestion_hover: hsla(217.0 / 360.0, 0.60, 0.50, 0.14),

            preview_card_bg: hsla(240.0 / 360.0, 0.10, 0.17, 1.0),
            preview_chrome_bg: hsla(0.0, 0.0, 1.0, 0.04),

            // #111113 — the mockup's `bg-elevated` for the sidebar.
            // Manifest in .tmp/gpui-html/theme.toml mirrors this hex.
            sidebar_bg: hsla(240.0 / 360.0, 0.056, 0.0706, 1.0),
            // #27272a — the mockup's `border-border`. Same hex as
            // chrome_border (Theme already exposes that field on a
            // separate PR; we duplicate the hex here so this slice
            // doesn't depend on it).
            sidebar_border: hsla(240.0 / 360.0, 0.037, 0.159, 1.0),
        }
    }
}
