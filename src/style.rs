//! Theme and shared styling helpers (GitHub-like look).

use iced::widget::{button, container, row, text};
use iced::{Background, Border, Color, Element, Length, Shadow, Theme, theme::Palette};

pub fn theme(dark: bool) -> Theme {
    let c = Color::from_rgb8;
    if dark {
        Theme::custom(
            "GitHub Dark",
            Palette {
                background: c(0x0d, 0x11, 0x17),
                text: c(0xe6, 0xed, 0xf3),
                primary: c(0x1f, 0x6f, 0xeb),
                success: c(0x23, 0x86, 0x36),
                warning: c(0xd2, 0x99, 0x22),
                danger: c(0xda, 0x36, 0x33),
            },
        )
    } else {
        Theme::custom(
            "GitHub Light",
            Palette {
                background: c(0xff, 0xff, 0xff),
                text: c(0x1f, 0x23, 0x28),
                primary: c(0x09, 0x69, 0xda),
                success: c(0x1a, 0x7f, 0x37),
                warning: c(0x9a, 0x67, 0x00),
                danger: c(0xcf, 0x22, 0x2e),
            },
        )
    }
}

pub fn is_dark(theme: &Theme) -> bool {
    theme.extended_palette().is_dark
}

pub fn subtle_bg(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0x15, 0x1b, 0x23) } else { Color::from_rgb8(0xf6, 0xf8, 0xfa) }
}

pub fn border_color(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0x30, 0x36, 0x3d) } else { Color::from_rgb8(0xd1, 0xd9, 0xe0) }
}

pub fn muted(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0x9d, 0xa7, 0xb3) } else { Color::from_rgb8(0x59, 0x63, 0x6e) }
}

pub fn green(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0x3f, 0xb9, 0x50) } else { Color::from_rgb8(0x1a, 0x7f, 0x37) }
}

pub fn red(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0xf8, 0x51, 0x49) } else { Color::from_rgb8(0xcf, 0x22, 0x2e) }
}

pub fn accent(theme: &Theme) -> Color {
    theme.palette().primary
}

pub fn purple(theme: &Theme) -> Color {
    if is_dark(theme) { Color::from_rgb8(0xa3, 0x71, 0xf7) } else { Color::from_rgb8(0x82, 0x50, 0xdf) }
}

/// Bordered "card" such as a file section or a panel.
pub fn card(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().background)),
        border: Border { color: border_color(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

/// Header strip of a card.
pub fn card_header(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(subtle_bg(theme))),
        border: Border { color: border_color(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

pub fn panel(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(subtle_bg(theme))),
        border: Border { color: border_color(theme), width: 1.0, radius: 0.0.into() },
        ..Default::default()
    }
}

pub fn bar(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(subtle_bg(theme))),
        border: Border { color: border_color(theme), width: 1.0, radius: 0.0.into() },
        ..Default::default()
    }
}

pub fn error_box_style(theme: &Theme) -> container::Style {
    let dark = is_dark(theme);
    container::Style {
        background: Some(Background::Color(if dark {
            Color::from_rgb8(0x49, 0x1b, 0x1d)
        } else {
            Color::from_rgb8(0xff, 0xeb, 0xe9)
        })),
        text_color: Some(if dark { Color::from_rgb8(0xff, 0xa1, 0x98) } else { Color::from_rgb8(0x82, 0x07, 0x1e) }),
        border: Border { color: red(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

pub fn info_box_style(theme: &Theme) -> container::Style {
    let dark = is_dark(theme);
    container::Style {
        background: Some(Background::Color(if dark {
            Color::from_rgba8(0x38, 0x8b, 0xfd, 0.15)
        } else {
            Color::from_rgb8(0xdd, 0xf4, 0xff)
        })),
        border: Border { color: accent(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

pub fn modal(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().background)),
        border: Border { color: border_color(theme), width: 1.0, radius: 8.0.into() },
        shadow: Shadow { color: Color::from_rgba8(0, 0, 0, 0.35), offset: iced::Vector::new(0.0, 8.0), blur_radius: 24.0 },
        ..Default::default()
    }
}

pub fn backdrop(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.45))),
        ..Default::default()
    }
}

pub fn menu(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().background)),
        border: Border { color: border_color(theme), width: 1.0, radius: 6.0.into() },
        shadow: Shadow { color: Color::from_rgba8(0, 0, 0, 0.25), offset: iced::Vector::new(0.0, 4.0), blur_radius: 12.0 },
        ..Default::default()
    }
}

pub fn bubble(theme: &Theme, user: bool) -> container::Style {
    let bg = if user {
        if is_dark(theme) { Color::from_rgba8(0x38, 0x8b, 0xfd, 0.18) } else { Color::from_rgb8(0xdd, 0xf4, 0xff) }
    } else {
        theme.palette().background
    };
    container::Style {
        background: Some(Background::Color(bg)),
        border: Border { color: border_color(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

pub fn pill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(Color { a: 0.15, ..color })),
        text_color: Some(color),
        border: Border { color: Color { a: 0.5, ..color }, width: 1.0, radius: 10.0.into() },
        ..Default::default()
    }
}

/// Flat, borderless button that only highlights on hover (for tree rows, icons).
pub fn flat_button(theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(if is_dark(theme) {
            Color::from_rgba8(0xff, 0xff, 0xff, 0.08)
        } else {
            Color::from_rgba8(0x00, 0x00, 0x00, 0.06)
        })),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: theme.palette().text,
        border: Border { radius: 6.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// GitHub's default (gray, bordered) button.
pub fn default_button(theme: &Theme, status: button::Status) -> button::Style {
    let dark = is_dark(theme);
    let base = if dark { Color::from_rgb8(0x21, 0x26, 0x2d) } else { Color::from_rgb8(0xf6, 0xf8, 0xfa) };
    let hover = if dark { Color::from_rgb8(0x29, 0x2e, 0x36) } else { Color::from_rgb8(0xee, 0xf1, 0xf4) };
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => base,
        })),
        text_color: if matches!(status, button::Status::Disabled) { muted(theme) } else { theme.palette().text },
        border: Border { color: border_color(theme), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

/// A [`default_button`] showing an on/off state: accent-tinted while on.
pub fn toggle_button(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut s = default_button(theme, status);
        if on {
            let a = accent(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let alpha = match (is_dark(theme), hovered) {
                (true, false) => 0.15,
                (true, true) => 0.25,
                (false, false) => 0.08,
                (false, true) => 0.14,
            };
            s.background = Some(Background::Color(Color { a: alpha, ..a }));
            s.border.color = a;
            s.text_color = a;
        }
        s
    }
}

pub fn small<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(12)
}

/// Red error box with a dismiss button.
pub fn error_box<'a, M: Clone + 'a>(msg: &'a str, on_dismiss: Option<M>) -> Element<'a, M> {
    let mut r = row![text("⚠").size(16), text(msg).size(13).width(Length::Fill)].spacing(10);
    if let Some(m) = on_dismiss {
        r = r.push(button(text("✕").size(12)).style(flat_button).padding([2, 6]).on_press(m));
    }
    container(r).padding(12).width(Length::Fill).style(error_box_style).into()
}

/// Markdown settings with GitHub-like inline code (subtle gray pill, normal text color).
pub fn markdown_settings(theme: &Theme) -> iced::widget::markdown::Settings {
    let mut style = iced::widget::markdown::Style::from_palette(theme.palette());
    style.inline_code_highlight = iced::widget::markdown::Highlight {
        background: Background::Color(Color::from_rgba8(0x81, 0x8b, 0x98, if is_dark(theme) { 0.4 } else { 0.15 })),
        border: Border { radius: 4.0.into(), ..Default::default() },
    };
    style.inline_code_color = theme.palette().text;
    style.inline_code_padding = iced::Padding::from([0, 3]);
    style.font = crate::fonts::ui();
    style.inline_code_font = crate::fonts::code();
    style.code_block_font = crate::fonts::code();
    iced::widget::markdown::Settings::with_text_size(14, style)
}

/// Pick lists used as dropdown menus: readable placeholder, GitHub button look.
pub fn menu_pick_list(theme: &Theme, status: iced::widget::pick_list::Status) -> iced::widget::pick_list::Style {
    let mut s = iced::widget::pick_list::default(theme, status);
    s.placeholder_color = theme.palette().text;
    s.text_color = theme.palette().text;
    s.background = Background::Color(match status {
        iced::widget::pick_list::Status::Hovered | iced::widget::pick_list::Status::Opened { .. } => {
            if is_dark(theme) { Color::from_rgb8(0x29, 0x2e, 0x36) } else { Color::from_rgb8(0xee, 0xf1, 0xf4) }
        }
        _ => subtle_bg(theme),
    });
    s.border = Border { color: border_color(theme), width: 1.0, radius: 6.0.into() };
    s
}
