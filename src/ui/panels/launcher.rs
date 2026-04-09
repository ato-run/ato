use gpui::prelude::*;
use gpui::{div, hsla, img, point, px, AnyElement, BoxShadow, IntoElement, ObjectFit};

pub(in crate::ui) fn render_launcher_panel() -> impl IntoElement {
    div()
        .relative()
        .size_full()
        // Background image — no overlay
        .child(
            img("bg_launcher.jpg")
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
        // Content layer — each element carries its own contrast
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(36.0))
                        .py(px(40.0))
                        .child(render_greeting())
                        .child(render_command_input())
                        .child(render_pinned_section())
                        .child(render_recent_section()),
                )
                .child(render_bottom_bar()),
        )
}

fn render_greeting() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(48.0))
                .font_weight(gpui::FontWeight(300.0))
                // Full white — readable on any background
                .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.6),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(12.0),
                    spread_radius: px(0.0),
                }])
                .child("Good morning"),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.70))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.5),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(6.0),
                    spread_radius: px(0.0),
                }])
                .child("Wednesday, April 8"),
        )
}

fn render_command_input() -> impl IntoElement {
    div()
        .w(px(540.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            // Search bar: light glass — stands out like Google's search bar
            div()
                .flex()
                .items_center()
                .h(px(46.0))
                .gap(px(12.0))
                .px(px(18.0))
                .bg(hsla(0.0, 0.0, 1.0, 0.14))
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.30))
                .rounded(px(18.0))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.35),
                    offset: point(px(0.0), px(2.0)),
                    blur_radius: px(16.0),
                    spread_radius: px(0.0),
                }])
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                        .child("⌘"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.0))
                        .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                        .child("Search, command, or ask AI…"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.0))
                        .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                        .px(px(8.0))
                        .py(px(3.0))
                        .bg(hsla(0.0, 0.0, 1.0, 0.10))
                        .border_1()
                        .border_color(hsla(0.0, 0.0, 1.0, 0.25))
                        .rounded(px(6.0))
                        .child("⌘ K"),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(16.0))
                .child(hint_item("⌘K", "Search"))
                .child(hint_item("⌘N", "New"))
                .child(hint_item("⌘O", "Overview")),
        )
}

fn hint_item(kbd: &'static str, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(16.0))
                .px(px(4.0))
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight(500.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.70))
                .bg(hsla(0.0, 0.0, 0.0, 0.40))
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.20))
                .rounded(px(3.0))
                .child(kbd),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.5),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(4.0),
                    spread_radius: px(0.0),
                }])
                .child(label),
        )
}

fn render_pinned_section() -> impl IntoElement {
    div()
        .w(px(540.0))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .child(section_header("PINNED", "Edit"))
        .child(
            div()
                .flex()
                .gap(px(10.0))
                .child(pinned_card("Design System", 217.0, 0.91, 0.60))
                .child(pinned_card("Strategy", 258.0, 0.92, 0.76))
                .child(pinned_card("Dev + Specs", 161.0, 0.62, 0.52))
                .child(pinned_card("QA Testing", 0.0, 0.91, 0.71)),
        )
        .child(
            div()
                .flex()
                .gap(px(10.0))
                .child(pinned_card("Research", 43.0, 0.96, 0.56))
                .child(pinned_card("Sprint Review", 25.0, 0.95, 0.61))
                .child(pinned_card("API Docs", 330.0, 0.81, 0.60))
                .child(pinned_card("Agent Branch", 258.0, 0.92, 0.76)),
        )
}

fn section_header(title: &'static str, action: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(2.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight(600.0))
                // Brighter — readable without overlay
                .text_color(hsla(0.0, 0.0, 1.0, 0.65))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.6),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(4.0),
                    spread_radius: px(0.0),
                }])
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.45))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.5),
                    offset: point(px(0.0), px(1.0)),
                    blur_radius: px(4.0),
                    spread_radius: px(0.0),
                }])
                .child(action),
        )
}

fn pinned_card(label: &'static str, hue_deg: f32, sat: f32, lit: f32) -> impl IntoElement {
    let hue = hue_deg / 360.0;
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .py(px(18.0))
        .px(px(8.0))
        .rounded(px(14.0))
        // Dark glass card — each card provides its own contrast
        .bg(hsla(240.0 / 360.0, 0.15, 0.05, 0.55))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.12))
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.30),
            offset: point(px(0.0), px(2.0)),
            blur_radius: px(10.0),
            spread_radius: px(0.0),
        }])
        .child(
            div()
                .w(px(42.0))
                .h(px(42.0))
                .rounded(px(10.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(hue, sat, lit, 0.25)),
        )
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight(500.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.80))
                .child(label),
        )
}

fn render_recent_section() -> impl IntoElement {
    div()
        .w(px(540.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .mb(px(12.0))
                .child(section_header("RECENT", "View all")),
        )
        .child(recent_item(
            "design-tokens.json",
            "Design System / Tokens",
            "3 min ago",
            217.0,
            0.91,
            0.60,
        ))
        .child(recent_item(
            "component-specs.tsx",
            "Design System / Components",
            "18 min ago",
            161.0,
            0.62,
            0.52,
        ))
        .child(recent_item(
            "spacing-scale.css",
            "Design System / Styles",
            "1h ago",
            43.0,
            0.96,
            0.56,
        ))
        .child(recent_item(
            "sprint-planning.md",
            "Strategy / Meetings",
            "3h ago",
            258.0,
            0.92,
            0.76,
        ))
}

fn recent_item(
    title: &'static str,
    desc: &'static str,
    time: &'static str,
    hue_deg: f32,
    sat: f32,
    lit: f32,
) -> impl IntoElement {
    let hue = hue_deg / 360.0;
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(10.0))
        // Each row has its own dark glass pill
        .bg(hsla(240.0 / 360.0, 0.12, 0.04, 0.50))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.08))
        .child(
            div()
                .w(px(28.0))
                .h(px(28.0))
                .rounded(px(6.0))
                .flex_shrink_0()
                .bg(hsla(hue, sat, lit, 0.28)),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(gpui::FontWeight(500.0))
                        .text_color(hsla(0.0, 0.0, 1.0, 0.85))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(hsla(0.0, 0.0, 1.0, 0.45))
                        .child(desc),
                ),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.35))
                .flex_shrink_0()
                .child(time),
        )
}

fn render_bottom_bar() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .pb(px(24.0))
        .child(bottom_action("+ New Workspace"))
        .child(bottom_action("↓ Import"))
        .child(bottom_action("⊞ Templates"))
        .child(bottom_action("⌨ Shortcuts"))
}

fn bottom_action(label: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(6.0))
        // Dark glass pill per action
        .bg(hsla(0.0, 0.0, 0.0, 0.40))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.12))
        .text_size(px(10.5))
        .text_color(hsla(0.0, 0.0, 1.0, 0.60))
        .child(label)
        .into_any_element()
}
