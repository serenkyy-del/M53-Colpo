//! Shared visual roles for application panels; actions remain native controls.
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::{prelude::*, *};

pub(crate) fn panel_heading(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .w_full()
        .h(px(32.))
        .flex_shrink_0()
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .text_color(cx.theme().secondary_foreground)
        .font_semibold()
        .child(title.into())
}

pub(crate) fn section_heading(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .w_full()
        .min_h(px(32.))
        .flex_shrink_0()
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .text_color(cx.theme().secondary_foreground)
        .font_semibold()
        .child(title.into())
}
