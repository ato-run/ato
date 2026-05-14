//! gpui-html → gpui Rust, by hand.
//!
//! Source mockups: `.tmp/header.html` (Chrome-style URL bar) and
//! `.tmp/sidebar.html` (Arc-style collapsible sidebar). Those files
//! are the gpuiHTML input; this file is what the gpui-html compiler
//! (https://github.com/ato-run/gpui-html) is intended to produce.
//!
//! Run from the ato-desktop crate root:
//!     cargo run --example gpui_html_mockup
//!
//! The demo composes both surfaces in one window: header pinned at
//! the top, sidebar pinned to the left, placeholder content area
//! filling the rest. The sidebar collapse toggle (top-right of the
//! sidebar header) actually works — everything else is visual.

use std::borrow::Cow;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, rgba, size, svg, Bounds, Context, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, SharedString, Styled, Window, WindowBounds, WindowDecorations,
    WindowOptions,
};
use gpui_component::{Icon, IconName, IconNamed};

// ----------------------------------------------------------------------
// Theme tokens — taken verbatim from the tailwind.config blocks at the
// top of header.html and sidebar.html. The two files disagree on
// `elevated` (header uses #18181b, sidebar uses #111113); we keep both
// because each surface uses its own.
// ----------------------------------------------------------------------

const BASE: u32 = 0x09090b;
const SURFACE: u32 = 0x09090b;
// header.html and sidebar.html define `elevated` differently
// (#18181b vs #111113). Header's `elevated` value coincides with
// `card`, so we only keep the sidebar variant explicitly.
const SIDEBAR_PANEL: u32 = 0x111113;
const CARD: u32 = 0x18181b;
const HOVER: u32 = 0x1f1f23;
const BORDER: u32 = 0x27272a;
const MEDIUM: u32 = 0x3f3f46;
const PRIMARY: u32 = 0xfafafa;
const SECONDARY: u32 = 0xd4d4d8;
const MUTED: u32 = 0x71717a;
const GHOST: u32 = 0x52525b;
const ACCENT: u32 = 0x6366f1;
const ACCENT_FG: u32 = 0xffffff;

// Traffic lights.
const TRAFFIC_ROSE: u32 = 0xff5f57;
const TRAFFIC_AMBER: u32 = 0xfebc2e;
const TRAFFIC_GREEN: u32 = 0x28c840;

// Sidebar language badges.
const LANG_PY: u32 = 0x38bdf8;
const LANG_JS: u32 = 0xfacc15;
const LANG_RS: u32 = 0xfb923c;
const LANG_GO: u32 = 0x22d3ee;

// `bg-rose/80`, `bg-accent/10`, `border-border/50` etc. become packed
// RGBA constants. Tailwind opacity → byte: /80 = 0xCC, /50 = 0x80,
// /20 = 0x33, /10 = 0x1A.
const BORDER_HALF: u32 = 0x27272a_80;
const ACCENT_10: u32 = 0x6366f1_1a;
const ACCENT_20: u32 = 0x6366f1_33;
const ACCENT_50: u32 = 0x6366f1_80;
const TRAFFIC_ROSE_80: u32 = 0xff5f57_cc;
const TRAFFIC_AMBER_80: u32 = 0xfebc2e_cc;
const TRAFFIC_GREEN_80: u32 = 0x28c840_cc;
const LANG_PY_10: u32 = 0x38bdf8_1a;
const LANG_JS_10: u32 = 0xfacc15_1a;
const LANG_RS_10: u32 = 0xfb923c_1a;
const LANG_GO_10: u32 = 0x22d3ee_1a;

// ----------------------------------------------------------------------
// AssetSource — delegates to gpui-component's bundled icon set, plus
// inlines the two SVGs the bundle is missing (`icons/lock.svg` and
// `icons/refresh-cw.svg`), so the demo is self-contained.
// ----------------------------------------------------------------------

const LOCK_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z"/></svg>"##;

const REFRESH_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"/></svg>"##;

const PANEL_TOGGLE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 19l-7-7 7-7m8 14l-7-7 7-7"/></svg>"##;

struct DemoAssets;

impl gpui::AssetSource for DemoAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match path {
            "icons/lock.svg" => Ok(Some(Cow::Borrowed(LOCK_SVG))),
            "icons/refresh-cw.svg" => Ok(Some(Cow::Borrowed(REFRESH_SVG))),
            "icons/panel-toggle.svg" => Ok(Some(Cow::Borrowed(PANEL_TOGGLE_SVG))),
            _ => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

// ----------------------------------------------------------------------
// Root view
// ----------------------------------------------------------------------

struct Mockup {
    sidebar_collapsed: bool,
    omnibar_value: SharedString,
}

impl Mockup {
    fn new() -> Self {
        Self {
            sidebar_collapsed: false,
            omnibar_value: SharedString::new_static("ato://home"),
        }
    }
}

impl Render for Mockup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Without an explicit rem size, gpui's `.text_sm()` /
        // `.size_8()` helpers (which lower to `rems(...)`) collapse
        // to zero — `gpui_component::Root` does this for production
        // ato-desktop, but we don't wrap in Root, so we set it here.
        window.set_rem_size(px(16.0));
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BASE))
            .text_color(rgb(PRIMARY))
            .child(render_header(&self.omnibar_value))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(render_sidebar(self.sidebar_collapsed, cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(SURFACE))
                            .text_color(rgb(MUTED))
                            .text_sm()
                            .child("Content Area"),
                    ),
            )
    }
}

// ----------------------------------------------------------------------
// header.html → gpui
// ----------------------------------------------------------------------

fn render_header(omnibar_value: &SharedString) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .h_12()
        .bg(rgb(SURFACE))
        .border_b_1()
        .border_color(rgb(BORDER))
        .px_4()
        .gap_2()
        .relative()
        // Traffic lights.
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .mr_2()
                .child(traffic_dot(TRAFFIC_ROSE_80, TRAFFIC_ROSE))
                .child(traffic_dot(TRAFFIC_AMBER_80, TRAFFIC_AMBER))
                .child(traffic_dot(TRAFFIC_GREEN_80, TRAFFIC_GREEN)),
        )
        // Nav controls (back / forward / reload).
        .child(
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .text_color(rgb(MUTED))
                .child(nav_button("hdr-back", IconName::ChevronLeft.path()))
                .child(nav_button("hdr-fwd", IconName::ChevronRight.path()))
                .child(nav_button("hdr-reload", "icons/refresh-cw.svg".into())),
        )
        // Omnibar.
        .child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .px_2()
                .child(render_omnibar(omnibar_value)),
        )
        // Right-side actions.
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(rgb(MUTED))
                .child(nav_button("hdr-plus", IconName::Plus.path()))
                .child(nav_button("hdr-bell", IconName::Bell.path()))
                .child(nav_button("hdr-settings", IconName::Settings.path()))
                .child(
                    // Vertical divider.
                    div()
                        .w_px()
                        .h_4()
                        .bg(rgb(BORDER))
                        .mx_1(),
                )
                .child(
                    // Avatar.
                    div()
                        .id("hdr-avatar")
                        .size_7()
                        .rounded_full()
                        .bg(rgb(ACCENT))
                        .text_color(rgb(ACCENT_FG))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.9))
                        .child("u"),
                ),
        )
}

fn traffic_dot(rest: u32, hover: u32) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("dot-{rest:08x}")))
        .size_3()
        .rounded_full()
        .bg(rgba(rest))
        .hover(move |s| s.bg(rgb(hover)))
}

fn nav_button(id: &'static str, icon_path: SharedString) -> impl IntoElement {
    div()
        .id(id)
        .p_1p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(SECONDARY)))
        .child(svg().path(icon_path).size_4().text_color(rgb(MUTED)))
}

fn render_omnibar(value: &SharedString) -> impl IntoElement {
    // Omnibar mirrors the HTML's `relative w-full max-w-xl flex
    // items-center` wrapper plus the absolutely-positioned leading
    // icon and the input pill itself.
    div()
        .relative()
        .w_full()
        .max_w(px(576.0)) // max-w-xl = 36rem
        .flex()
        .items_center()
        .child(
            // Leading lock icon, absolutely positioned.
            div()
                .absolute()
                .left_3()
                .text_color(rgb(MUTED))
                .child(
                    svg()
                        .path(SharedString::new_static("icons/lock.svg"))
                        .size(px(14.0))
                        .text_color(rgb(MUTED)),
                ),
        )
        .child(
            div()
                .id("omnibar-pill")
                .w_full()
                .h_8()
                .bg(rgb(CARD))
                .border_1()
                .border_color(gpui::transparent_black())
                .rounded_full()
                .pl_8()
                .pr_4()
                .flex()
                .items_center()
                .text_sm()
                .text_color(rgb(SECONDARY))
                .cursor_text()
                .hover(|s| s.bg(rgb(HOVER)).border_color(rgb(MEDIUM)))
                .child(value.clone()),
        )
}

// ----------------------------------------------------------------------
// sidebar.html → gpui
// ----------------------------------------------------------------------

#[derive(Copy, Clone)]
struct TabSpec {
    id: &'static str,
    label: &'static str,
    badge: &'static str,
    badge_fg: u32,
    badge_bg: u32,
    active: bool,
}

const TABS: &[TabSpec] = &[
    TabSpec {
        id: "tab-libretranslate",
        label: "libretranslate",
        badge: "Py",
        badge_fg: LANG_PY,
        badge_bg: LANG_PY_10,
        active: true,
    },
    TabSpec {
        id: "tab-image-resizer",
        label: "image-resizer",
        badge: "JS",
        badge_fg: LANG_JS,
        badge_bg: LANG_JS_10,
        active: false,
    },
    TabSpec {
        id: "tab-mdbook",
        label: "mdbook-renderer",
        badge: "Rs",
        badge_fg: LANG_RS,
        badge_bg: LANG_RS_10,
        active: false,
    },
    TabSpec {
        id: "tab-sqlite",
        label: "sqlite-explorer",
        badge: "Go",
        badge_fg: LANG_GO,
        badge_bg: LANG_GO_10,
        active: false,
    },
];

fn render_sidebar(collapsed: bool, cx: &mut Context<Mockup>) -> impl IntoElement {
    let width = if collapsed { px(72.0) } else { px(256.0) }; // 4.5rem / 16rem

    div()
        .flex()
        .flex_col()
        .h_full()
        .w(width)
        .flex_shrink_0()
        .bg(rgb(SIDEBAR_PANEL))
        .border_r_1()
        .border_color(rgb(BORDER))
        .overflow_hidden()
        // Header row: New Tab / Search / Toggle.
        .child(render_sidebar_header(collapsed, cx))
        // Tab list.
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .px_2()
                .pt_3()
                .pb_1()
                .overflow_y_hidden()
                .children(TABS.iter().map(|t| render_tab(*t, collapsed))),
        )
        // Footer: Settings.
        .child(
            div()
                .mt_auto()
                .border_t_1()
                .border_color(rgba(BORDER_HALF))
                .py_2()
                .px_2()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(render_settings_row(collapsed)),
        )
}

fn render_sidebar_header(collapsed: bool, cx: &mut Context<Mockup>) -> impl IntoElement {
    let mut row = div()
        .flex()
        .items_center()
        .h_12()
        .px_3()
        .gap_1()
        .border_b_1()
        .border_color(rgba(BORDER_HALF));

    if collapsed {
        row = row.justify_center().px_0();
    }

    if !collapsed {
        row = row
            .child(sidebar_header_button(
                "side-newtab",
                IconName::Plus.path(),
                "New Tab",
            ))
            .child(sidebar_header_button(
                "side-search",
                IconName::Search.path(),
                "Search",
            ))
            .child(div().flex_1());
    }

    row.child(
        div()
            .id("side-toggle")
            .p_1p5()
            .rounded_md()
            .cursor_pointer()
            .text_color(rgb(GHOST))
            .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(SECONDARY)))
            .child(
                svg()
                    .path(SharedString::new_static("icons/panel-toggle.svg"))
                    .size_4(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                cx.notify();
            })),
    )
}

fn sidebar_header_button(
    id: &'static str,
    icon_path: SharedString,
    label: &'static str,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .p_1p5()
        .rounded_md()
        .flex_shrink_0()
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(SECONDARY)))
        .child(svg().path(icon_path).size_4())
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
}

fn render_tab(tab: TabSpec, collapsed: bool) -> impl IntoElement {
    // Wrapper provides the Arc-style left accent indicator and active
    // tint background.
    let mut item = div()
        .id(tab.id)
        .relative()
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer();

    if tab.active {
        item = item.bg(rgba(ACCENT_10));
    } else {
        item = item.hover(|s| s.bg(rgb(HOVER)));
    }

    if collapsed {
        item = item.justify_center().px_0().mx_1();
    }

    if tab.active && !collapsed {
        // 3px left accent rail (the `::before` pseudo-element).
        item = item.child(
            div()
                .absolute()
                .left_0()
                .top(gpui::relative(0.25))
                .bottom(gpui::relative(0.25))
                .w(px(3.0))
                .bg(rgb(ACCENT))
                .rounded_r(px(4.0)),
        );
    }

    let badge: Hsla = rgb(tab.badge_fg).into();

    item = item.child(
        div()
            .size_8()
            .rounded_lg()
            .bg(rgba(tab.badge_bg))
            .text_color(badge)
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .child(tab.badge),
    );

    if !collapsed {
        let label_color = if tab.active { rgb(ACCENT) } else { rgb(SECONDARY) };
        item = item.child(
            div()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(label_color)
                        .truncate()
                        .child(tab.label),
                ),
        );

        // Close button: visible only on hover. We use group hover so
        // hovering the row reveals the X.
        let (close_color, close_hover_bg, close_hover_color) = if tab.active {
            (rgba(ACCENT_50), rgba(ACCENT_20), rgb(ACCENT))
        } else {
            (rgb(GHOST).into(), rgb(HOVER).into(), rgb(PRIMARY))
        };

        item = item.group("tab-row").child(
            div()
                .id(SharedString::from(format!("close-{}", tab.id)))
                .opacity(0.0)
                .group_hover("tab-row", |s| s.opacity(1.0))
                .p_0p5()
                .rounded(px(4.0))
                .text_color(close_color)
                .cursor_pointer()
                .hover(move |s| s.bg(close_hover_bg).text_color(close_hover_color))
                .child(Icon::new(IconName::Close).size(px(12.0))),
        );
    }

    item
}

fn render_settings_row(collapsed: bool) -> impl IntoElement {
    let mut row = div()
        .id("side-settings")
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .text_color(rgb(MUTED))
        .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(SECONDARY)));

    if collapsed {
        row = row.justify_center().px_0().mx_1();
    }

    row = row.child(
        div()
            .size_8()
            .rounded_lg()
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .child(svg().path(IconName::Settings.path()).size_4()),
    );

    if !collapsed {
        row = row.child(
            div()
                .flex_1()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child("Settings"),
        );
    }

    row
}

// ----------------------------------------------------------------------
// main
// ----------------------------------------------------------------------

fn main() {
    let app = gpui_platform::application().with_assets(DemoAssets);
    app.run(|cx| {
        gpui_component::init(cx);

        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_decorations: Some(WindowDecorations::Server),
                focus: true,
                show: true,
                ..Default::default()
            },
            |_, cx| cx.new(|_| Mockup::new()),
        )
        .unwrap();

        cx.activate(true);
    });
}
