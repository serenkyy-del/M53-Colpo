//! Explicit theme selection takes precedence over OS changes for this session.
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::{App, Window};
use std::sync::atomic::{AtomicBool, Ordering};

static MANUAL: AtomicBool = AtomicBool::new(false);

/// Instructional help has its own quiet surface, separate from warnings.
pub(crate) struct GuidanceColors {
    pub background: gpui_kit::Hsla,
    pub foreground: gpui_kit::Hsla,
    pub secondary_foreground: gpui_kit::Hsla,
    pub border: gpui_kit::Hsla,
    pub hover: gpui_kit::Hsla,
    pub active: gpui_kit::Hsla,
}

pub(crate) fn guidance_colors(cx: &App) -> GuidanceColors {
    let dark = cx.theme().is_dark();
    let color = |light, dark_value| gpui_kit::rgb(if dark { dark_value } else { light }).into();
    GuidanceColors {
        background: color(0xfff8dc, 0x393426),
        foreground: color(0x51472a, 0xf4e7bc),
        secondary_foreground: color(0x6b5d35, 0xd8c997),
        border: gpui_kit::rgb(0xa18b45).into(),
        hover: color(0xf5ebc5, 0x49412b),
        active: color(0xeadcb0, 0x574d30),
    }
}

/// Preserve the catalog hue while keeping small colored text legible on either theme.
pub fn marker_text(value: u32, cx: &App) -> gpui_kit::Hsla {
    let mut color: gpui_kit::Hsla = gpui_kit::rgb(value).into();
    color.l = if cx.theme().is_dark() {
        color.l.max(0.70)
    } else {
        color.l.min(0.36)
    };
    color
}

pub fn sync_system(window: Option<&mut Window>, cx: &mut App) {
    if !MANUAL.load(Ordering::Relaxed) {
        Theme::sync_system_appearance(window, cx);
    }
    contrast(cx);
}

pub fn choose(dark: bool, window: &mut Window, cx: &mut App) {
    MANUAL.store(true, Ordering::Relaxed);
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        Some(window),
        cx,
    );
    contrast(cx);
}

pub(crate) fn contrast(cx: &mut App) {
    Theme::update(cx, |theme| {
        let dark = theme.is_dark();
        let color = |value| gpui_kit::rgb(value).into();
        theme.colors.border = color(if dark { 0x687383 } else { 0x8793a3 });
        theme.colors.input = theme.colors.border;
        theme.colors.muted_foreground = color(if dark { 0xc4cbd5 } else { 0x465365 });
        theme.colors.accordion = color(if dark { 0x252e3a } else { 0xe4eaf2 });
        theme.colors.secondary = theme.colors.accordion;
        theme.colors.secondary_foreground = theme.colors.foreground;
        // Keep selection visible after focus moves to the patient's study panel.
        theme.colors.table_active = color(if dark { 0x164e75 } else { 0xbfdbfe });
        theme.colors.table_active_border = color(if dark { 0x7dd3fc } else { 0x1d4ed8 });
    });
}
