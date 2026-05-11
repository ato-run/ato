mod favicon_links;

pub(super) use favicon_links::parse_link_icon_candidates;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    div, img, linear_color_stop, linear_gradient, point, px, BoxShadow, Div, FontWeight, Image,
    InteractiveElement, IntoElement, MouseButton, Stateful, Window,
};
use gpui_component::{Icon, IconName};

use super::current_rail_width;
use super::theme::{task_hue, Theme};
use crate::app::{
    CloseTask, FocusCommandBar, MoveTask, NewTab, SelectTask, ShowSettings, ToggleSidebar,
};
use crate::state::{HostPanelRoute, PaneSurface};

/// Drag-and-drop payload for reordering sidebar task tabs. The drop
/// handler reads `task_id` to dispatch `MoveTask { task_id, to_index }`
/// where `to_index` is the position of the tab the payload was dropped
/// onto. `from_index` is unused by the handler (we look up the source
/// position in state) but kept for diagnostics. `ghost` carries a
/// fully resolved snapshot of the tab's icon (favicon image, monogram
/// label, etc.) so the drag preview renders the same glyph as the
/// rail icon without needing access to the live Theme / favicon cache
/// inside the GPUI Render impl.
#[derive(Clone, Debug)]
pub(super) struct DraggedTaskTab {
    pub task_id: usize,
    pub from_index: usize,
    pub ghost: GhostIcon,
}

#[derive(Clone, Debug)]
pub(super) struct GhostIcon {
    pub kind: GhostIconKind,
    pub colors: GhostIconColors,
}

#[derive(Clone, Debug)]
pub(super) enum GhostIconKind {
    Monogram { label: String, hue: f32 },
    Favicon(Arc<Image>),
    Globe,
    SystemIcon(SystemPageIcon),
}

#[derive(Clone, Debug)]
pub(super) struct GhostIconColors {
    pub border: gpui::Hsla,
    pub surface: gpui::Hsla,
    pub text_tertiary: gpui::Hsla,
}

impl GhostIcon {
    fn from_spec(
        spec: &SidebarTaskIconSpec,
        seed: u64,
        favicon_cache: &HashMap<String, FaviconState>,
        theme: &Theme,
    ) -> Self {
        let kind = match spec {
            SidebarTaskIconSpec::Monogram(label) => GhostIconKind::Monogram {
                label: label.clone(),
                hue: task_hue(seed),
            },
            SidebarTaskIconSpec::ExternalUrl { origin } => match favicon_cache.get(origin) {
                Some(FaviconState::Ready(image)) => GhostIconKind::Favicon(image.clone()),
                _ => GhostIconKind::Globe,
            },
            SidebarTaskIconSpec::Image { source } => match favicon_cache.get(source) {
                Some(FaviconState::Ready(image)) => GhostIconKind::Favicon(image.clone()),
                _ => GhostIconKind::Monogram {
                    label: "●".to_string(),
                    hue: task_hue(seed),
                },
            },
            SidebarTaskIconSpec::SystemIcon(page_type) => {
                GhostIconKind::SystemIcon(page_type.clone())
            }
        };
        Self {
            kind,
            colors: GhostIconColors {
                border: theme.border_default,
                surface: theme.surface_hover,
                text_tertiary: theme.text_tertiary,
            },
        }
    }
}

impl gpui::Render for DraggedTaskTab {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        // 36×36 chip that mirrors the rail icon — fully opaque so
        // the user can see exactly which tab they grabbed.
        div()
            .w(px(NAV_ITEM_SIZE))
            .h(px(NAV_ITEM_SIZE))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .child(render_ghost_icon(&self.ghost))
    }
}

fn render_ghost_icon(ghost: &GhostIcon) -> Div {
    match &ghost.kind {
        GhostIconKind::Monogram { label, hue } => {
            // Mirror render_monogram_icon's fixed saturation/lightness
            // so the drag preview matches the rail icon exactly.
            let saturation = 0.55;
            let lightness = 0.50;
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(gpui::hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
                    linear_color_stop(
                        gpui::hsla(
                            ((hue + 30.0) % 360.0) / 360.0,
                            saturation * 0.9,
                            lightness * 0.85,
                            1.0,
                        ),
                        1.,
                    ),
                ))
                .border_1()
                .border_color(ghost.colors.border)
                .text_color(gpui::white())
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(monogram_label(label))
        }
        GhostIconKind::Favicon(image) => div()
            .w(px(APP_ICON_SIZE))
            .h(px(APP_ICON_SIZE))
            .rounded(px(5.0))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .bg(ghost.colors.surface)
            .border_1()
            .border_color(ghost.colors.border)
            .child(img(image.clone()).size_full()),
        GhostIconKind::Globe => div()
            .w(px(APP_ICON_SIZE))
            .h(px(APP_ICON_SIZE))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(ghost.colors.surface)
            .border_1()
            .border_color(ghost.colors.border)
            .text_color(ghost.colors.text_tertiary)
            .child(Icon::new(IconName::Globe).size(px(14.0))),
        GhostIconKind::SystemIcon(page_type) => {
            let (label, hue) = match page_type {
                SystemPageIcon::Console => (">_", 270.0),
                SystemPageIcon::Terminal => ("$", 160.0),
                SystemPageIcon::Launcher => ("◆", 217.0),
                SystemPageIcon::Inspector => ("i", 45.0),
                SystemPageIcon::CapsuleStatus => ("⊙", 0.0),
            };
            let saturation = 0.55_f32;
            let lightness = 0.50_f32;
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(hue / 360.0, saturation, lightness, 0.15))
                .border_1()
                .border_color(ghost.colors.border)
                .text_color(gpui::hsla(
                    hue / 360.0,
                    saturation + 0.1,
                    lightness + 0.2,
                    1.0,
                ))
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.to_string())
        }
    }
}

fn monogram_label(raw: &str) -> String {
    raw.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}
use crate::state::{AppState, SidebarTaskIconSpec, SidebarTaskItem, SystemPageIcon};

const NAV_ITEM_SIZE: f32 = 36.0;
const APP_ICON_SIZE: f32 = 22.0;

#[derive(Clone)]
pub(super) enum FaviconState {
    /// In flight. Carries the number of times this origin has previously
    /// failed so the post-fetch resolver can bump the count if we fail
    /// again — without that, a permanently broken origin would retry on
    /// every `retry_delay` interval forever.
    Loading {
        prior_attempts: u32,
    },
    Ready(Arc<Image>),
    Failed {
        failed_at: Instant,
        attempts: u32,
    },
}

/// Cap on how many times we'll retry a single origin's favicon before
/// giving up permanently. The render path falls back to the globe glyph
/// once we stop retrying, which is preferable to issuing a request to
/// the same broken URL every 10 seconds for the lifetime of the app.
pub(super) const MAX_FAVICON_ATTEMPTS: u32 = 5;

impl FaviconState {
    pub(super) fn should_fetch(&self, now: Instant, retry_delay: Duration) -> bool {
        match self {
            Self::Loading { .. } | Self::Ready(_) => false,
            Self::Failed {
                failed_at,
                attempts,
            } => *attempts < MAX_FAVICON_ATTEMPTS && now.duration_since(*failed_at) >= retry_delay,
        }
    }
}

pub(super) fn render_task_rail(
    state: &AppState,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> impl IntoElement {
    let tasks = state.sidebar_task_items();
    let panel_bg = theme.panel_bg;
    let panel_border = theme.panel_border;
    // Width is dynamic: collapsed (72px) by default, expanded (256px)
    // when state.sidebar_expanded is true. The same helper feeds
    // compute_stage_bounds so WebView positioning stays in sync.
    let rail_width = current_rail_width(state);
    let expanded = state.sidebar_expanded;

    // Outer container = the rail surface. Header (h-12) and body
    // (flex_1) are stacked vertically — the body keeps the previous
    // py_3 / gap_1 / items_center rhythm so existing tabs render
    // identically below the new header.
    div()
        .w(px(rail_width))
        .min_w(px(rail_width))
        .h_full()
        .flex()
        .flex_col()
        .bg(panel_bg)
        .border_r_1()
        .border_color(panel_border)
        .child(render_sidebar_header(expanded, theme))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .w_full()
                .py_3()
                .gap_1()
                .children(
                    tasks.into_iter().enumerate().map(move |(i, task)| {
                        render_nav_item(task, i, expanded, favicon_cache, theme)
                    }),
                )
                .child(render_nav_separator(theme))
                .child(div().flex_1())
                .child(render_settings_nav_item(settings_nav_active(state), theme)),
        )
}

/// Sidebar header — Plus (NewTab) + Search (FocusCommandBar) icon
/// buttons in a 48px tall row at the top of the rail.
///
/// ## gpui-html origin
///
/// Lowered from `.tmp/gpui-html/sidebar-header.html`. The generated
/// chain is preserved in `.tmp/gpui-html/sidebar-header.generated.rs`.
/// The mockup expresses two `p-1 rounded-md` boxes wrapping a
/// `size-4` icon slot inside an `h-12 gap-1` flex row with a
/// `border-b` separator; production replaces the placeholder
/// `div().size_4()` with real `Icon::new(IconName::{Plus,Search})`
/// and adds hover state + click handlers (out of gpui-html v0.1's
/// static-lowering scope).
///
/// Replaces the previous `render_new_tab_button` that sat mid-rail
/// between the task list and the spacer.
fn render_sidebar_header(expanded: bool, theme: &Theme) -> impl IntoElement {
    // Toggle icon flips with state so the affordance shows what
    // clicking would do: collapsed → PanelLeftOpen (action: expand),
    // expanded → PanelLeftClose (action: collapse).
    let toggle_icon = if expanded {
        IconName::PanelLeftClose
    } else {
        IconName::PanelLeftOpen
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .h_12()
        .gap_1()
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(render_sidebar_header_button(
            "sidebar-header-new-tab",
            IconName::Plus,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(NewTab), cx);
            },
        ))
        .child(render_sidebar_header_button(
            "sidebar-header-search",
            IconName::Search,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(FocusCommandBar), cx);
            },
        ))
        .child(render_sidebar_header_button(
            "sidebar-header-toggle",
            toggle_icon,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(ToggleSidebar), cx);
            },
        ))
}

fn render_sidebar_header_button(
    id: &'static str,
    icon: IconName,
    theme: &Theme,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    // Same visual treatment as the chrome action / nav buttons —
    // text-tertiary rest, text-secondary + surface-hover bg on
    // hover. p-1 instead of p-1.5 because 72px rail can't fit two
    // p-1.5 buttons + gap comfortably; p-1 (4px each side) lands
    // each button at 24px, two of them + gap-1 = 52px, centered on
    // the 72px rail with 10px breathing room.
    div()
        .id(id)
        .p_1()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text_tertiary)
        .cursor_pointer()
        .hover(move |s| s.bg(theme.surface_hover).text_color(theme.text_secondary))
        .on_mouse_down(MouseButton::Left, on_click)
        .child(Icon::new(icon).size_4())
}

/// Ordered favicon candidate URLs to try for a given origin.
///
/// Modern Vite / Next.js / static-site setups frequently ship only
/// `favicon.svg` or `apple-touch-icon.png` — the legacy `/favicon.ico`
/// 404s. Returning an ordered fallback list lets the fetcher accept
/// the first candidate that responds with a real image.
///
/// Returns an empty `Vec` for non-`http(s)` origins (e.g. `file://`,
/// `capsule://`) so the fetcher short-circuits without issuing a
/// request that would never make sense.
pub(super) fn favicon_candidate_urls(origin: &str) -> Vec<String> {
    let Ok(parsed) = url::Url::parse(origin) else {
        return Vec::new();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Vec::new();
    }
    vec![
        format!("{origin}/favicon.ico"),
        format!("{origin}/favicon.svg"),
        format!("{origin}/apple-touch-icon.png"),
    ]
}

fn render_nav_item(
    task: SidebarTaskItem,
    index: usize,
    expanded: bool,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> Stateful<Div> {
    let task_id = task.id;
    let title = task.title.clone();
    let is_active = task.is_active;
    let accent_subtle = theme.accent_subtle;
    let accent = theme.accent;
    let drag_id = (task_id, index);

    let mut item = div()
        .id(("task-tab", task_id))
        // Group lets the close-button child react to the rail item's
        // hover state (canonical gpui-component pattern, see
        // notification.rs). The empty group name matches the close
        // button's group_hover() call below.
        .group("")
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .relative()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.dispatch_action(Box::new(SelectTask { task_id }), cx);
        })
        .on_drag(
            DraggedTaskTab {
                task_id: drag_id.0,
                from_index: drag_id.1,
                ghost: GhostIcon::from_spec(&task.icon, task_id as u64, favicon_cache, theme),
            },
            |dragged, _offset, _window, cx| {
                cx.stop_propagation();
                cx.new(|_| dragged.clone())
            },
        )
        .on_drop(move |dragged: &DraggedTaskTab, window, cx| {
            if dragged.task_id == task_id {
                return;
            }
            window.dispatch_action(
                Box::new(MoveTask {
                    task_id: dragged.task_id,
                    to_index: index,
                }),
                cx,
            );
        });

    // Collapsed = the original 36×36 square. Expanded = horizontal
    // row that fills the rail with icon + label + close button,
    // matching the mockup's `.tab-item` block (sidebar.html lines
    // 182-189).
    if expanded {
        item = item.w_full().px_2().gap_3();
    } else {
        item = item.w(px(NAV_ITEM_SIZE)).justify_center();
    }

    // Active accent bar — position differs between modes:
    //   collapsed: existing left(-8) anchor outside the tab
    //   expanded : mockup-faithful left:0 inside the tab at 50% height
    let item = if is_active {
        if expanded {
            item.bg(accent_subtle).child(
                div()
                    .absolute()
                    .left_0()
                    .top(gpui::relative(0.25))
                    .bottom(gpui::relative(0.25))
                    .w(px(3.0))
                    .rounded_r(px(3.0))
                    .bg(accent),
            )
        } else {
            item.bg(accent_subtle).child(
                div()
                    .absolute()
                    .left(px(-8.0))
                    .top_1_2()
                    .mt(px(-9.0))
                    .w(px(3.0))
                    .h(px(18.0))
                    .rounded_r(px(3.0))
                    .bg(accent),
            )
        }
    } else {
        item
    };

    let mut item = item.child(render_app_icon(
        task.icon,
        task_id as u64,
        favicon_cache,
        theme,
    ));

    // Expanded mode shows the task title between the icon and the
    // close button. Active tabs use accent color; idle tabs use
    // text_secondary (matches the mockup's `text-accent` /
    // `text-secondary` swap on `.tab-item.active`).
    if expanded {
        let label_color = if is_active {
            theme.accent
        } else {
            theme.text_secondary
        };
        item = item.child(
            div().flex_1().overflow_hidden().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight(500.0))
                    .text_color(label_color)
                    .truncate()
                    .child(title),
            ),
        );
    }

    item.child(render_close_button(task_id, theme))
}

fn render_close_button(task_id: usize, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("task-close", task_id))
        // Hidden by default, revealed only while the parent rail
        // item is hovered. Kept clickable when visible.
        .invisible()
        .group_hover("", |this| this.visible())
        .absolute()
        .top(px(-4.0))
        .right(px(-4.0))
        .w(px(14.0))
        .h(px(14.0))
        .rounded_full()
        .bg(theme.panel_bg)
        .border_1()
        .border_color(theme.border_default)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(
            Icon::new(IconName::Close)
                .size(px(8.0))
                .text_color(theme.text_secondary),
        )
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(CloseTask { task_id }), cx);
        })
}

fn render_app_icon(
    icon: SidebarTaskIconSpec,
    seed: u64,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> Div {
    match icon {
        SidebarTaskIconSpec::Monogram(label) => render_monogram_icon(&label, task_hue(seed), theme),
        SidebarTaskIconSpec::ExternalUrl { origin } => match favicon_cache.get(&origin) {
            Some(FaviconState::Ready(image)) => render_favicon_icon(image.clone(), theme),
            Some(FaviconState::Loading { .. }) | Some(FaviconState::Failed { .. }) | None => {
                render_globe_icon(theme)
            }
        },
        SidebarTaskIconSpec::Image { source } => match favicon_cache.get(&source) {
            // The favicon cache doubles as the pane-icon cache —
            // both are keyed by the request URL/path, both produce
            // an `Arc<Image>`, and both render through
            // `render_favicon_icon`. Until the bytes are loaded we
            // fall back to the same monogram tile that capsules
            // without an icon would show, so the slot never goes
            // empty.
            Some(FaviconState::Ready(image)) => render_favicon_icon(image.clone(), theme),
            Some(FaviconState::Loading { .. }) | Some(FaviconState::Failed { .. }) | None => {
                render_monogram_icon("●", task_hue(seed), theme)
            }
        },
        SidebarTaskIconSpec::SystemIcon(page_type) => render_system_icon(page_type, theme),
    }
}

fn render_monogram_icon(label: &str, hue: f32, theme: &Theme) -> Div {
    // Active/inactive selection is communicated by the surrounding chip
    // (accent_subtle backdrop + accent rail bar) — the icon itself stays
    // identical so the same task reads as the same color across the
    // sidebar, drag preview, and any other surface.
    let saturation = 0.55_f32;
    let lightness = 0.50_f32;
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(gpui::hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
            linear_color_stop(
                gpui::hsla(
                    ((hue + 30.0) % 360.0) / 360.0,
                    saturation * 0.9,
                    lightness * 0.85,
                    1.0,
                ),
                1.,
            ),
        ))
        .shadow(vec![BoxShadow {
            color: gpui::hsla(hue / 360.0, saturation, lightness, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        }])
        .border_1()
        .border_color(border_color)
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(gpui::hsla(0.0, 0.0, 1.0, 1.0))
        .child(label.to_string())
}

fn render_favicon_icon(image: Arc<Image>, theme: &Theme) -> Div {
    // White chip behind the favicon: SVG / PNG icons are routinely
    // designed against a white card (browser tab strips, OS docks) and
    // ato.run / Google Material icons in particular invert badly on the
    // panel-tinted `surface_hover`. Keeping the chip white matches the
    // canonical browser tab rendering and keeps transparent areas of
    // the icon legible.
    let bg = ICON_CHIP_BG;
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .child(img(image).size_full())
}

fn render_globe_icon(theme: &Theme) -> Div {
    // Globe placeholder uses the same white chip as `render_favicon_icon`
    // so the rail stays visually uniform whether or not a given origin
    // resolves a favicon.
    let bg = ICON_CHIP_BG;
    let border_color = theme.border_default;
    let text_color = theme.text_tertiary;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .text_color(text_color)
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .child("◎")
}

/// Solid white chip behind favicon images and the globe placeholder.
/// Decoupled from `theme.surface_hover` because favicons are designed
/// against white in the browser tab strip — re-tinting them with the
/// panel hover color makes Google / MDN-style colored marks read as
/// muddy gray. See `render_favicon_icon` / `render_globe_icon`.
const ICON_CHIP_BG: gpui::Hsla = gpui::Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};

fn render_system_icon(page_type: SystemPageIcon, theme: &Theme) -> Div {
    // Hue is per-role (Terminal=green, Console=purple, …) rather than
    // per-identity, so these icons read the same regardless of state.
    let (label, hue) = match page_type {
        SystemPageIcon::Console => (">_", 270.0),    // purple
        SystemPageIcon::Terminal => ("$", 160.0),    // green
        SystemPageIcon::Launcher => ("◆", 217.0),    // blue
        SystemPageIcon::Inspector => ("i", 45.0),    // yellow
        SystemPageIcon::CapsuleStatus => ("⊙", 0.0), // red
    };

    let saturation = 0.55_f32;
    let lightness = 0.50_f32;
    let bg = gpui::hsla(hue / 360.0, saturation, lightness, 0.20);
    let text_color = gpui::hsla(hue / 360.0, saturation + 0.1, lightness + 0.2, 1.0);
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .text_color(text_color)
        .text_size(px(10.0))
        .font_weight(FontWeight::BOLD)
        .child(label)
}

fn render_nav_separator(theme: &Theme) -> Div {
    div()
        .w(px(24.0))
        .h(px(1.0))
        .bg(theme.border_subtle)
        .my_1p5()
}

#[allow(dead_code)]
fn render_branch_section() -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(14.0))
                .text_color(gpui::rgb(0xa78bfa))
                .child("⑂"),
        )
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(gpui::hsla(0.0, 0.0, 1.0, 0.10)),
        )
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(gpui::rgb(0xa78bfa))
                .cursor_pointer()
                .shadow(vec![BoxShadow {
                    color: gpui::hsla(270.0 / 360.0, 0.73, 0.73, 0.3),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(4.),
                    spread_radius: px(0.),
                }]),
        )
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(gpui::hsla(0.0, 0.0, 1.0, 0.10)),
        )
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .border_1()
                .border_color(gpui::rgb(0xa78bfa))
                .cursor_pointer(),
        )
}

fn render_settings_nav_item(active: bool, theme: &Theme) -> Div {
    let bg = if active {
        theme.accent_subtle
    } else {
        theme.surface_hover
    };
    let icon_color = if active {
        theme.text_primary
    } else {
        theme.text_tertiary
    };

    div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(ShowSettings), cx);
        })
        .child(
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(bg)
                .text_size(px(14.0))
                .text_color(icon_color)
                .child("⚙"),
        )
}

fn settings_nav_active(state: &crate::state::AppState) -> bool {
    if state.settings_panel_open {
        return true;
    }

    state
        .active_task()
        .map(|task| {
            task.panes.iter().any(|pane| {
                matches!(
                    pane.surface,
                    PaneSurface::HostPanel(HostPanelRoute::Settings { .. })
                )
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{favicon_candidate_urls, FaviconState, MAX_FAVICON_ATTEMPTS};
    use std::time::{Duration, Instant};

    #[test]
    fn favicon_candidates_cover_modern_default_assets() {
        // Order matters: legacy .ico first (most universal), then SVG
        // (Vite/Next.js default), then apple-touch-icon (iOS-friendly).
        // Modern static-site frameworks frequently ship only `.svg` or
        // `apple-touch-icon.png`, so the .ico fallback alone leaves the
        // sidebar showing a globe glyph for those origins.
        assert_eq!(
            favicon_candidate_urls("https://example.com"),
            vec![
                "https://example.com/favicon.ico".to_string(),
                "https://example.com/favicon.svg".to_string(),
                "https://example.com/apple-touch-icon.png".to_string(),
            ]
        );
        assert_eq!(
            favicon_candidate_urls("http://localhost:3000"),
            vec![
                "http://localhost:3000/favicon.ico".to_string(),
                "http://localhost:3000/favicon.svg".to_string(),
                "http://localhost:3000/apple-touch-icon.png".to_string(),
            ]
        );
        // Non-http(s) origins must yield no candidates so the fetcher
        // short-circuits without issuing nonsense requests.
        assert!(favicon_candidate_urls("file:///tmp/app").is_empty());
        assert!(favicon_candidate_urls("capsule://ato.run/koh0920/x").is_empty());
    }

    #[test]
    fn favicon_state_failed_entries_retry_after_backoff() {
        let retry_delay = Duration::from_secs(10);
        let now = Instant::now();

        let loading = FaviconState::Loading { prior_attempts: 0 };
        assert!(!loading.should_fetch(now, retry_delay));

        let failed_recently = FaviconState::Failed {
            failed_at: now - Duration::from_secs(3),
            attempts: 1,
        };
        assert!(!failed_recently.should_fetch(now, retry_delay));

        let failed_long_ago = FaviconState::Failed {
            failed_at: now - Duration::from_secs(12),
            attempts: 1,
        };
        assert!(failed_long_ago.should_fetch(now, retry_delay));
    }

    #[test]
    fn favicon_state_caps_retries_at_max_attempts() {
        // Past the cap, even a long-elapsed Failed entry must NOT
        // re-fetch — otherwise a permanently broken origin (404 across
        // every fallback URL) would generate a request every retry_delay
        // for the lifetime of the app.
        let retry_delay = Duration::from_secs(10);
        let now = Instant::now();
        let exhausted = FaviconState::Failed {
            failed_at: now - Duration::from_secs(120),
            attempts: MAX_FAVICON_ATTEMPTS,
        };
        assert!(!exhausted.should_fetch(now, retry_delay));

        // One short of the cap, still retriable after the backoff.
        let almost_exhausted = FaviconState::Failed {
            failed_at: now - Duration::from_secs(120),
            attempts: MAX_FAVICON_ATTEMPTS - 1,
        };
        assert!(almost_exhausted.should_fetch(now, retry_delay));
    }
}
