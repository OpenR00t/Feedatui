// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Drawing: title list on the left, the article on the right.

use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::app::{App, Focus, PickerRow, Popup};
use crate::article::{Block as ABlock, truncate, wrap};
use crate::images::{ImgState, fit_size};
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(theme::page()), area);

    let [main, bar] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let list_w = (u32::from(main.width) * 32 / 100).clamp(28, 60) as u16;
    let [left, right] = Layout::horizontal([
        Constraint::Length(list_w.min(main.width / 2)),
        Constraint::Min(10),
    ])
    .areas(main);
    app.list_area = left;
    app.article_area = right;

    draw_list(f, app, left);
    draw_article(f, app, right);
    draw_bar(f, app, bar);

    match &app.popup {
        Popup::Groups { cursor, expanded } => {
            let rows = app.picker_rows(expanded);
            draw_picker(f, app, &rows, *cursor, expanded);
        }
        Popup::Help => draw_help(f),
        Popup::None => {}
    }
}

fn panel<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(title)
}

fn ago(t: Option<chrono::DateTime<Utc>>) -> String {
    let Some(t) = t else { return String::new() };
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..2_592_000 => format!("{}d", s / 86_400),
        _ => t
            .with_timezone(&chrono::Local)
            .format("%e %b %Y")
            .to_string(),
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List;
    let unread = app
        .visible
        .iter()
        .filter_map(|r| app.item(r))
        .filter(|i| !app.store.read.contains(&i.id))
        .count();
    let mut title = vec![Span::styled(
        format!(" {} ", app.scope_name()),
        theme::title(focused),
    )];
    if app.unread_only {
        title.push(Span::styled("· unread ", theme::meta()));
    }
    let count = if app.visible.is_empty() {
        String::new()
    } else {
        format!(
            " {}/{} · {} new ",
            app.list.selected().map_or(0, |s| s + 1),
            app.visible.len(),
            unread
        )
    };
    let block = panel(Line::from(title), focused)
        .title_bottom(Line::styled(count, theme::meta()).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.visible.is_empty() {
        let msg = if !app.in_flight.is_empty() {
            "Fetching feeds…"
        } else if !app.filter.is_empty() {
            "Nothing matches the filter."
        } else if app.unread_only {
            "All caught up."
        } else {
            "No articles yet. Press r to refresh."
        };
        f.render_widget(
            Paragraph::new(Line::styled(msg, theme::meta())).alignment(Alignment::Center),
            Rect {
                y: inner.y + inner.height / 2,
                height: 1,
                ..inner
            },
        );
        return;
    }

    let w = usize::from(inner.width);
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .filter_map(|r| app.item(r))
        .map(|it| {
            let is_read = app.store.read.contains(&it.id);
            let dot = if is_read {
                Span::raw("  ")
            } else {
                Span::styled("● ", Style::default().fg(theme::ACCENT))
            };
            let title_style = if is_read {
                Style::default().fg(theme::FG4)
            } else {
                Style::default().fg(theme::FG1).add_modifier(Modifier::BOLD)
            };
            let when = ago(it.published);
            let source = app.feed_name(&it.feed_url);
            let src_w = w.saturating_sub(when.len() + 4).max(1);
            let source = truncate(&source, src_w);
            let pad = w.saturating_sub(
                2 + unicode_width::UnicodeWidthStr::width(source.as_str()) + when.len(),
            );
            ListItem::new(vec![
                Line::from(vec![
                    dot,
                    Span::styled(truncate(&it.title, w.saturating_sub(2)), title_style),
                ]),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(source, theme::source()),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(when, theme::meta()),
                ]),
            ])
        })
        .collect();
    let list = List::new(items).highlight_style(theme::selected(focused));
    f.render_stateful_widget(list, inner, &mut app.list);
}

fn draw_article(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Article;
    let block = panel(Line::styled(" Article ", theme::title(focused)), focused);
    let inner = block.inner(area);
    // One column of padding each side.
    let content = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if content.width < 4 || content.height < 1 {
        f.render_widget(block, area);
        return;
    }

    let Some(article) = app.article(content.width).cloned() else {
        f.render_widget(block, area);
        let msg = if app.visible.is_empty() {
            ""
        } else {
            "Select an article"
        };
        f.render_widget(
            Paragraph::new(Line::styled(msg, theme::meta())).alignment(Alignment::Center),
            Rect {
                y: inner.y + inner.height / 2,
                height: 1,
                ..inner
            },
        );
        return;
    };

    // Lay out: each block's height in rows.
    let max_img_h = content.height.saturating_sub(2).max(4);
    enum Placed {
        Text(Line<'static>),
        Image {
            url: String,
            size: Size,
            caption: Vec<String>,
        },
        Note(Line<'static>),
    }
    let mut placed: Vec<(usize, usize, Placed)> = Vec::new(); // (top, height, what)
    let mut y = 0usize;
    for b in article.blocks {
        match b {
            ABlock::Text(line) => {
                placed.push((y, 1, Placed::Text(line)));
                y += 1;
            }
            ABlock::Image { url, alt } => {
                let state = app.images.as_ref().and_then(|i| i.state(&url));
                match state {
                    Some(ImgState::Ready(img)) => {
                        let font = app
                            .images
                            .as_ref()
                            .map_or(ratatui_image::FontSize::new(10, 20), |i| i.font_size());
                        let size = fit_size(img, font, content.width, max_img_h);
                        let caption = wrap(&alt, usize::from(content.width));
                        let h = usize::from(size.height) + caption.len();
                        placed.push((y, h, Placed::Image { url, size, caption }));
                        y += h;
                    }
                    Some(ImgState::Loading) => {
                        placed.push((
                            y,
                            1,
                            Placed::Note(Line::styled("◌ loading image…", theme::meta())),
                        ));
                        y += 1;
                    }
                    Some(ImgState::Failed) | None => {
                        let text = if app.images.is_none() {
                            if alt.is_empty() {
                                "[image]".to_string()
                            } else {
                                format!("[image: {alt}]")
                            }
                        } else if alt.is_empty() {
                            continue;
                        } else {
                            format!("[image: {alt}]")
                        };
                        placed.push((y, 1, Placed::Note(Line::styled(text, theme::meta()))));
                        y += 1;
                    }
                }
            }
        }
    }
    app.article_rows = y;
    let page = usize::from(content.height);
    app.scroll = app.scroll.min(y.saturating_sub(page));
    let scroll = app.scroll;
    // Terminal graphics (sixel especially) can paint over popups, so images
    // step aside while one is open.
    let hide_images = !matches!(app.popup, Popup::None);

    let pct = if y <= page {
        String::new()
    } else {
        format!(" {}% ", (scroll + page).min(y) * 100 / y)
    };
    let block = block.title_bottom(Line::styled(pct, theme::meta()).alignment(Alignment::Right));
    f.render_widget(block, area);

    for (top, h, what) in placed {
        if top + h <= scroll || top >= scroll + page {
            continue;
        }
        let rel = top as isize - scroll as isize;
        match what {
            Placed::Text(line) | Placed::Note(line) => {
                let r = Rect {
                    y: content.y + rel as u16,
                    height: 1,
                    ..content
                };
                f.render_widget(Paragraph::new(line), r);
            }
            Placed::Image { url, size, caption } => {
                let x_off = (content.width - size.width) / 2;
                let proto = if hide_images {
                    None
                } else {
                    app.images.as_mut().and_then(|i| i.protocol(&url, size))
                };
                match proto {
                    Some(p) => {
                        let pos = SignedPosition::from((x_off as i16, rel as i16));
                        f.render_widget(SlicedImage::new(p, pos), content);
                    }
                    None => {
                        // Encoding (or hidden): hold the space so the layout doesn't jump.
                        let first = rel.max(0) as u16;
                        let last = (rel + size.height as isize).min(page as isize) as u16;
                        if last > first {
                            let r = Rect::new(
                                content.x + x_off,
                                content.y + first,
                                size.width,
                                last - first,
                            );
                            f.render_widget(
                                Block::default().style(Style::default().bg(theme::BG0_H)),
                                r,
                            );
                        }
                    }
                }
                for (n, c) in caption.into_iter().enumerate() {
                    let cy = rel + size.height as isize + n as isize;
                    if cy >= 0 && (cy as usize) < page {
                        let r = Rect {
                            y: content.y + cy as u16,
                            height: 1,
                            ..content
                        };
                        f.render_widget(
                            Paragraph::new(Line::styled(
                                c,
                                theme::meta().add_modifier(Modifier::ITALIC),
                            ))
                            .alignment(Alignment::Center),
                            r,
                        );
                    }
                }
            }
        }
    }

    if y > page {
        let mut state = ScrollbarState::new(y.saturating_sub(page)).position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_style(Style::default().fg(if focused { theme::ACCENT } else { theme::BG3 })),
            Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(2),
                ..area
            },
            &mut state,
        );
    }
}

fn hint<'a>(key: &'a str, what: &'a str) -> [Span<'a>; 2] {
    [
        Span::styled(key, theme::key()),
        Span::styled(format!(" {what}  "), theme::meta()),
    ]
}

fn draw_bar(f: &mut Frame, app: &mut App, area: Rect) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG0_H)),
        area,
    );

    if app.filtering || !app.filter.is_empty() {
        let mut spans = vec![
            Span::styled(" /", theme::key()),
            Span::styled(app.filter.clone(), Style::default().fg(theme::FG0)),
        ];
        if app.filtering {
            spans.push(Span::styled("▏", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled("   enter keep · esc clear", theme::meta()));
        } else {
            spans.push(Span::styled("   esc clears the filter", theme::meta()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let mut spans = vec![Span::raw(" ")];
    for (k, w) in [
        ("j/k", "move"),
        ("h/l", "pane"),
        ("o", "open"),
        ("b", "groups"),
        ("r", "refresh"),
        ("?", "help"),
    ] {
        spans.extend(hint(k, w));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    // Right side: what's going on.
    let right = if !app.in_flight.is_empty() {
        Some(Span::styled(
            format!("Refreshing… {} left ", app.in_flight.len()),
            Style::default().fg(theme::BLUE),
        ))
    } else if let Some(s) = app.status.as_ref().filter(|s| s.at.elapsed().as_secs() < 6) {
        Some(Span::styled(
            format!("{} ", s.text),
            if s.error {
                theme::error()
            } else {
                theme::meta()
            },
        ))
    } else if !app.feed_errors.is_empty() {
        Some(Span::styled(
            format!("{} feed(s) failed · b to see ", app.feed_errors.len()),
            theme::warning(),
        ))
    } else {
        None
    };
    if let Some(span) = right {
        let w = (span.width() as u16).min(area.width / 2 + 10);
        let r = Rect {
            x: area.right().saturating_sub(w),
            width: w,
            ..area
        };
        let text = truncate(&span.content, usize::from(w));
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(Span::styled(text, span.style))
                .style(Style::default().bg(theme::BG0_H))
                .alignment(Alignment::Right),
            r,
        );
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_picker(
    f: &mut Frame,
    app: &App,
    rows: &[PickerRow],
    cursor: usize,
    expanded: &std::collections::HashSet<usize>,
) {
    let height = (rows.len() as u16 + 4).min(f.area().height.saturating_sub(2));
    let area = centered(f.area(), 54, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .style(Style::default().bg(theme::BG0_H))
        .title(Line::styled(" Feed groups ", theme::title(true)))
        .title_bottom(
            Line::from(vec![
                Span::styled(" enter", theme::key()),
                Span::styled(" pick  ", theme::meta()),
                Span::styled("l/h", theme::key()),
                Span::styled(" feeds  ", theme::meta()),
                Span::styled("esc", theme::key()),
                Span::styled(" close ", theme::meta()),
            ])
            .alignment(Alignment::Center),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let w = usize::from(inner.width);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let unread = app.unread_for(*row);
            let (label, prefix, style) = match *row {
                PickerRow::All => (
                    "All feeds".to_string(),
                    " 0  ".to_string(),
                    Style::default().fg(theme::FG0).add_modifier(Modifier::BOLD),
                ),
                PickerRow::Group(g) => {
                    let arrow = if expanded.contains(&g) { "▾" } else { "▸" };
                    let num = if g < 9 {
                        format!("{}", g + 1)
                    } else {
                        " ".into()
                    };
                    (
                        format!("{arrow} {}", app.cfg.groups[g].name),
                        format!(" {num}  "),
                        Style::default().fg(theme::FG0).add_modifier(Modifier::BOLD),
                    )
                }
                PickerRow::Feed(g, fi) => {
                    let url = &app.cfg.groups[g].feeds[fi].url;
                    let failed = app.feed_errors.contains_key(url);
                    (
                        format!("{}{}", app.feed_name(url), if failed { "  ✗" } else { "" }),
                        "       ".to_string(),
                        if failed {
                            theme::error()
                        } else {
                            theme::source()
                        },
                    )
                }
            };
            let count = if unread > 0 {
                unread.to_string()
            } else {
                String::new()
            };
            let room = w.saturating_sub(prefix.chars().count() + count.len() + 2);
            let label = truncate(&label, room);
            let pad = room.saturating_sub(unicode_width::UnicodeWidthStr::width(label.as_str()));
            let mut line = Line::from(vec![
                Span::styled(prefix, theme::meta()),
                Span::styled(label, style),
                Span::raw(" ".repeat(pad + 1)),
                Span::styled(count, Style::default().fg(theme::ACCENT)),
            ]);
            if n == cursor {
                line = line.style(Style::default().bg(theme::BG1));
            }
            ListItem::new(line)
        })
        .collect();
    let mut state = ratatui::widgets::ListState::default().with_selected(Some(cursor));
    let list_area = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    f.render_stateful_widget(List::new(items), list_area, &mut state);
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        (
            "j / k  ↓ / ↑",
            "next / previous title, or scroll the article",
        ),
        (
            "h / l  ← / →",
            "focus the list / the article (also Tab, Enter)",
        ),
        ("J / K", "next / previous article from either pane"),
        ("gg / G", "top / bottom"),
        ("Ctrl-d / Ctrl-u", "half page down / up"),
        ("Ctrl-f / Ctrl-b", "page down / up (also Space)"),
        ("o", "open the article's link in the browser"),
        ("b", "feed group picker (l/h show a group's feeds)"),
        ("gt / gT", "next / previous group"),
        ("r / R", "refresh this group / everything"),
        ("m", "toggle read"),
        ("x", "show unread only"),
        ("/", "filter by title or feed name (Esc clears)"),
        ("mouse", "wheel scrolls, click picks a title or pane"),
        ("q", "quit"),
    ];
    let area = centered(f.area(), 66, rows.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .style(Style::default().bg(theme::BG0_H))
        .title(Line::styled(" Keys ", theme::title(true)))
        .title_bottom(Line::styled(" any key closes ", theme::meta()).alignment(Alignment::Center));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<17}"), theme::key()),
                Span::styled(*d, Style::default().fg(theme::FG1)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        },
    );
}
