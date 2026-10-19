// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Gruvbox dark palette and the job each color does in the UI.

use ratatui::style::{Color, Modifier, Style};

pub const BG0_H: Color = Color::Rgb(0x1d, 0x20, 0x21);
pub const BG0: Color = Color::Rgb(0x28, 0x28, 0x28);
pub const BG1: Color = Color::Rgb(0x3c, 0x38, 0x36);
pub const BG2: Color = Color::Rgb(0x50, 0x49, 0x45);
pub const BG3: Color = Color::Rgb(0x66, 0x5c, 0x54);
pub const BG4: Color = Color::Rgb(0x7c, 0x6f, 0x64);
pub const FG0: Color = Color::Rgb(0xfb, 0xf1, 0xc7);
pub const FG1: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
pub const FG3: Color = Color::Rgb(0xbd, 0xae, 0x93);
pub const FG4: Color = Color::Rgb(0xa8, 0x99, 0x84);
pub const GRAY: Color = Color::Rgb(0x92, 0x83, 0x74);
pub const RED: Color = Color::Rgb(0xfb, 0x49, 0x34);
pub const GREEN: Color = Color::Rgb(0xb8, 0xbb, 0x26);
pub const YELLOW: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
pub const BLUE: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const AQUA: Color = Color::Rgb(0x8e, 0xc0, 0x7c);
pub const ORANGE: Color = Color::Rgb(0xfe, 0x80, 0x19);

/// Primary accent: focus, selection, headings, unread markers.
pub const ACCENT: Color = GREEN;
/// Secondary accent: feed names, sub-headings.
pub const ACCENT_2: Color = AQUA;

pub fn page() -> Style {
    Style::default().bg(BG0).fg(FG1)
}

pub fn border(focused: bool) -> Style {
    Style::default().fg(if focused { ACCENT } else { BG4 })
}

pub fn title(focused: bool) -> Style {
    let s = Style::default().fg(if focused { ACCENT } else { FG4 });
    if focused {
        s.add_modifier(Modifier::BOLD)
    } else {
        s
    }
}

pub fn selected(focused: bool) -> Style {
    Style::default().bg(BG1).fg(if focused { FG0 } else { FG1 })
}

pub fn meta() -> Style {
    Style::default().fg(GRAY)
}

pub fn source() -> Style {
    Style::default().fg(ACCENT_2)
}

pub fn key() -> Style {
    Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
}

pub fn link() -> Style {
    Style::default().fg(BLUE).add_modifier(Modifier::UNDERLINED)
}

pub fn error() -> Style {
    Style::default().fg(RED)
}

pub fn warning() -> Style {
    Style::default().fg(YELLOW)
}
