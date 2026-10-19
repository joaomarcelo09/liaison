//! Colors and widget styles. Everything derives from two custom palettes so light and dark
//! mode stay consistent.

use iced::{
    Background, Border, Color, Font, Shadow, Theme, border,
    theme::{Palette, palette::Extended},
    widget::{button, container, text_editor, text_input},
};
use liaison_core::{NotifyLevel, SessionStatus, config::Desktop as DesktopConfig};

/// The UI font family: `[desktop] font` in config.toml, else the first installed of a few
/// common sans-serif families. Static fonts only: iced cannot pick weights out of variable
/// fonts (Inter, Adwaita Sans, …), so bold text would fall back to a monospace font.
pub fn ui_font() -> Font {
    static FAMILY: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    let family = FAMILY.get_or_init(|| {
        let configured = liaison_core::config::Config::load().desktop.font;
        let candidates = configured.into_iter().chain(
            ["Noto Sans", "DejaVu Sans", "Liberation Sans", "Open Sans", "Roboto"].map(String::from),
        );
        let installed = installed_families();
        candidates
            .into_iter()
            .find(|c| installed.iter().any(|f| f.eq_ignore_ascii_case(c)))
            .map(|f| &*Box::leak(f.into_boxed_str()))
    });
    match family {
        Some(name) => Font::with_name(name),
        None => Font::DEFAULT,
    }
}

/// Chat text size in pixels, set from `[desktop] text_size`; every other text size scales with it.
static TEXT_SIZE: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(DesktopConfig::DEFAULT_TEXT_SIZE);

pub fn set_text_size(size: u16) {
    TEXT_SIZE.store(size, std::sync::atomic::Ordering::Relaxed);
}

/// A text size from the layout (designed around 14px chat text), scaled to the user's choice.
pub fn fs(size: u16) -> f32 {
    f32::from(size) * f32::from(TEXT_SIZE.load(std::sync::atomic::Ordering::Relaxed)) / 14.0
}

fn installed_families() -> Vec<String> {
    std::process::Command::new("fc-list")
        .args([":", "family"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .flat_map(|l| l.split(','))
                .map(|f| f.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn dark() -> Theme {
    Theme::custom(
        "Liaison Dark",
        Palette {
            background: rgb(0x16181d),
            text: rgb(0xe6e8ee),
            primary: rgb(0x5b8cff),
            success: rgb(0x3fb96f),
            warning: rgb(0xe0a63a),
            danger: rgb(0xe5534b),
        },
    )
}

pub fn light() -> Theme {
    Theme::custom(
        "Liaison Light",
        Palette {
            background: rgb(0xf6f7f9),
            text: rgb(0x1d2129),
            primary: rgb(0x2f6bed),
            success: rgb(0x23944f),
            warning: rgb(0xb7791f),
            danger: rgb(0xd1383d),
        },
    )
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn ext(theme: &Theme) -> &Extended {
    theme.extended_palette()
}

pub fn muted(theme: &Theme) -> Color {
    let p = ext(theme);
    Color { a: 0.62, ..p.background.base.text }
}

pub fn status_color(theme: &Theme, status: SessionStatus) -> Color {
    let p = ext(theme);
    match status {
        SessionStatus::Active => p.primary.base.color,
        SessionStatus::Listening => p.success.base.color,
        SessionStatus::Idle => p.success.weak.color,
        SessionStatus::Disconnected => p.warning.base.color,
        SessionStatus::Ended => muted(theme),
    }
}

pub fn level_color(theme: &Theme, level: NotifyLevel) -> Color {
    let p = ext(theme);
    match level {
        NotifyLevel::Info => p.primary.base.color,
        NotifyLevel::Success => p.success.base.color,
        NotifyLevel::Warning => p.warning.base.color,
        NotifyLevel::Error => p.danger.base.color,
    }
}

pub fn sidebar(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.background.weak.color.scale_alpha(if p.is_dark { 0.35 } else { 0.6 }).into()),
        ..Default::default()
    }
}

pub fn header(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.background.base.color.into()),
        border: Border { color: p.background.strong.color.scale_alpha(0.5), width: 0.0, radius: 0.0.into() },
        ..Default::default()
    }
}

pub fn session_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = ext(theme);
        let background = match (selected, status) {
            (true, _) => Some(p.primary.weak.color.scale_alpha(if p.is_dark { 0.35 } else { 0.25 })),
            (false, button::Status::Hovered) => Some(p.background.strong.color.scale_alpha(0.35)),
            _ => None,
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: p.background.base.text,
            border: border::rounded(8),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

pub fn bubble_agent(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(if p.is_dark { p.background.weak.color } else { Color::WHITE }.into()),
        border: Border { color: p.background.strong.color.scale_alpha(0.4), width: 1.0, radius: 12.0.into() },
        text_color: Some(p.background.base.text),
        ..Default::default()
    }
}

pub fn bubble_user(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.primary.base.color.into()),
        border: border::rounded(12),
        text_color: Some(p.primary.base.text),
        ..Default::default()
    }
}

pub fn bubble_failed(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.danger.weak.color.scale_alpha(0.5).into()),
        border: Border { color: p.danger.base.color, width: 1.0, radius: 12.0.into() },
        text_color: Some(p.background.base.text),
        ..Default::default()
    }
}

/// A card with a colored accent border (notifications, questions, permissions).
pub fn card(accent: Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let p = ext(theme);
        container::Style {
            background: Some(
                if p.is_dark { p.background.weak.color } else { Color::WHITE }.into(),
            ),
            border: Border { color: accent, width: 1.5, radius: 12.0.into() },
            text_color: Some(p.background.base.text),
            ..Default::default()
        }
    }
}

pub fn banner(accent: Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let p = ext(theme);
        container::Style {
            background: Some(accent.scale_alpha(if p.is_dark { 0.16 } else { 0.12 }).into()),
            border: Border { color: accent.scale_alpha(0.5), width: 1.0, radius: 8.0.into() },
            text_color: Some(p.background.base.text),
            ..Default::default()
        }
    }
}

pub fn badge(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        border: border::rounded(9),
        text_color: Some(Color::WHITE),
        ..Default::default()
    }
}

pub fn pill(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.background.strong.color.scale_alpha(0.45).into()),
        border: border::rounded(6),
        text_color: Some(p.background.base.text),
        ..Default::default()
    }
}

pub fn dot(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style { background: Some(color.into()), border: border::rounded(5), ..Default::default() }
}

pub fn composer_box(theme: &Theme) -> container::Style {
    let p = ext(theme);
    container::Style {
        background: Some(p.background.base.color.into()),
        border: Border { color: p.background.strong.color.scale_alpha(0.5), width: 1.0, radius: 0.0.into() },
        ..Default::default()
    }
}

pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let mut style = text_editor::default(theme, status);
    style.border = style.border.rounded(10);
    style
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    style.border = style.border.rounded(8);
    style
}

/// Small flat button (icons, secondary actions).
pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let p = ext(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(p.background.strong.color.scale_alpha(0.45).into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: match status {
            button::Status::Disabled => muted(theme).scale_alpha(0.6),
            _ => p.background.base.text,
        },
        border: border::rounded(6),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn pill_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    style.border = style.border.rounded(8);
    style
}

pub fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::primary(theme, status);
    style.border = style.border.rounded(8);
    style
}

pub fn success_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::success(theme, status);
    style.border = style.border.rounded(8);
    style
}

pub fn danger_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::danger(theme, status);
    style.border = style.border.rounded(8);
    style
}
