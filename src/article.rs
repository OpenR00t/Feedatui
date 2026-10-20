// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Turns an article into styled paragraphs and images ([`Doc`]), then lays
//! them out for a pane ([`layout`]): the lead image sits in the top-left
//! corner and the text wraps around it.

use html2text::render::{TaggedLine, TaggedLineElement, TextDecorator};
use ratatui::layout::Size;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::feed::Item;
use crate::theme;

// Private-use markers the decorator writes so lines can be styled afterwards.
// (Quote prefixes stay ASCII: html2text measures them in bytes.)
const HEADING: char = '\u{E000}';
const IMAGE: char = '\u{E002}';
/// html2text renders at this width so each paragraph stays one line; the
/// layout does the wrapping.
const UNWRAPPED: usize = 4000;
/// Columns between the lead image and the text beside it.
const GAP: u16 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Ann {
    #[default]
    Plain,
    Link(String),
    Image(String),
    Emphasis,
    Strong,
    Strike,
    Code,
    Pre,
}

#[derive(Clone)]
struct Decorator;

impl TextDecorator for Decorator {
    type Annotation = Ann;

    fn decorate_link_start(&mut self, url: &str) -> (String, Ann) {
        (String::new(), Ann::Link(url.to_string()))
    }
    fn decorate_link_end(&mut self) -> String {
        String::new()
    }
    fn decorate_em_start(&self) -> (String, Ann) {
        (String::new(), Ann::Emphasis)
    }
    fn decorate_em_end(&self) -> String {
        String::new()
    }
    fn decorate_strong_start(&self) -> (String, Ann) {
        (String::new(), Ann::Strong)
    }
    fn decorate_strong_end(&self) -> String {
        String::new()
    }
    fn decorate_strikeout_start(&self) -> (String, Ann) {
        (String::new(), Ann::Strike)
    }
    fn decorate_strikeout_end(&self) -> String {
        String::new()
    }
    fn decorate_code_start(&self) -> (String, Ann) {
        (String::new(), Ann::Code)
    }
    fn decorate_code_end(&self) -> String {
        String::new()
    }
    fn decorate_preformat_first(&self) -> Ann {
        Ann::Pre
    }
    fn decorate_preformat_cont(&self) -> Ann {
        Ann::Pre
    }
    fn decorate_image(&mut self, src: &str, title: &str) -> (String, Ann) {
        // Never empty, so images without alt text survive rendering.
        (format!("{IMAGE}{title}"), Ann::Image(src.to_string()))
    }
    fn header_prefix(&self, level: usize) -> String {
        format!("{} ", HEADING.to_string().repeat(level.max(1)))
    }
    fn quote_prefix(&self) -> String {
        "> ".to_string()
    }
    fn unordered_item_prefix(&self) -> String {
        "• ".to_string()
    }
    fn ordered_item_prefix(&self, i: i64) -> String {
        format!("{i}. ")
    }
    fn make_subblock_decorator(&self) -> Self {
        self.clone()
    }
}

/// A paragraph, blank line or image of the article body.
#[derive(Debug, Clone)]
pub enum Block {
    Para {
        /// Quote bars, indent and bullet before the first line.
        prefix: Vec<Span<'static>>,
        /// What wrapped lines start with (quote bars and indent).
        cont: Vec<Span<'static>>,
        spans: Vec<Span<'static>>,
        /// Preformatted text and tables are clipped, never wrapped.
        wrap: bool,
    },
    Blank,
    Image {
        url: String,
        alt: String,
    },
}

/// An article ready to lay out at any width.
#[derive(Debug, Clone)]
pub struct Doc {
    pub title: String,
    pub meta: Vec<Span<'static>>,
    pub link: Option<String>,
    /// A line about the full-article fetch, under the link.
    pub note: Option<Line<'static>>,
    pub lead: Option<(String, String)>,
    pub blocks: Vec<Block>,
    /// Every image URL, lead first.
    pub images: Vec<String>,
}

/// Builds the article from `body` (the feed's HTML or the fetched page),
/// resolving relative links against `base` (default: the article link).
pub fn build(item: &Item, source: &str, body: &str, base: Option<&str>) -> Doc {
    let mut meta = vec![Span::styled(source.to_string(), theme::source())];
    if let Some(a) = &item.author {
        meta.push(Span::styled(" · ", theme::meta()));
        meta.push(Span::styled(a.clone(), Style::default().fg(theme::FG3)));
    }
    if let Some(p) = item.published {
        let local = p.with_timezone(&chrono::Local);
        meta.push(Span::styled(
            format!(" · {}", local.format("%a %e %b %Y, %H:%M")),
            theme::meta(),
        ));
    }

    let html = if looks_like_html(body) {
        crate::html::clean(body)
    } else {
        plain_to_html(body)
    };
    let base = base
        .or(item.link.as_deref())
        .and_then(|l| reqwest::Url::parse(l).ok());
    let mut blocks = render_html(&html, base.as_ref());

    // The lead image: the body's first image if nothing comes before it,
    // else the feed's own picture unless the body already shows it.
    let first_content = blocks.iter().position(|b| !matches!(b, Block::Blank));
    let lead = match first_content {
        Some(i) if matches!(blocks[i], Block::Image { .. }) => match blocks.remove(i) {
            Block::Image { url, alt } => Some((url, alt)),
            _ => None,
        },
        _ => item.image.as_ref().and_then(|img| {
            let shown = blocks
                .iter()
                .any(|b| matches!(b, Block::Image { url, .. } if same_image(url, img)));
            (!shown).then(|| (crate::html::shrink_cdn(img), String::new()))
        }),
    };
    while matches!(blocks.first(), Some(Block::Blank)) {
        blocks.remove(0);
    }
    if blocks.is_empty() {
        blocks.push(Block::Para {
            prefix: vec![],
            cont: vec![],
            spans: vec![Span::styled(
                "This feed has no article text. Press f to fetch the full article or o to open it in the browser.",
                theme::meta().add_modifier(Modifier::ITALIC),
            )],
            wrap: true,
        });
    }

    let mut images: Vec<String> = Vec::new();
    if let Some((u, _)) = &lead {
        images.push(u.clone());
    }
    for b in &blocks {
        if let Block::Image { url, .. } = b
            && !images.contains(url)
        {
            images.push(url.clone());
        }
    }

    Doc {
        title: item.title.clone(),
        meta,
        link: item.link.clone(),
        note: None,
        lead,
        blocks,
        images,
    }
}

/// Same picture at a different size: equal ignoring the query string.
fn same_image(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.split(['?', '#']).next().unwrap_or(s).to_string();
    strip(a) == strip(b)
}

fn looks_like_html(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('<') || t.contains("</") || t.contains("<br") || t.contains("<img")
}

fn plain_to_html(s: &str) -> String {
    let esc = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    esc.split("\n\n")
        .map(|p| format!("<p>{}</p>", p.trim().replace('\n', "<br>")))
        .collect()
}

fn render_html(html: &str, base: Option<&reqwest::Url>) -> Vec<Block> {
    let lines = html2text::config::with_decorator(Decorator)
        .empty_img_mode(html2text::config::ImageRenderMode::ShowAlways)
        .lines_from_read(html.as_bytes(), UNWRAPPED)
        .unwrap_or_default();

    let mut out: Vec<Block> = Vec::new();
    for line in lines {
        for block in convert_line(&line, base) {
            // An image whose alt text html2text split over lines: join it.
            if let Block::Image { url, alt } = &block
                && let Some(Block::Image {
                    url: prev,
                    alt: prev_alt,
                }) = out.last_mut()
                && prev == url
            {
                if !alt.is_empty() {
                    if !prev_alt.is_empty() {
                        prev_alt.push(' ');
                    }
                    prev_alt.push_str(alt);
                }
                continue;
            }
            // Collapse runs of blank lines.
            if matches!(block, Block::Blank) && matches!(out.last(), Some(Block::Blank) | None) {
                continue;
            }
            out.push(block);
        }
    }
    while matches!(out.last(), Some(Block::Blank)) {
        out.pop();
    }
    out
}

fn resolve(src: &str, base: Option<&reqwest::Url>) -> Option<String> {
    if src.starts_with("data:") || src.is_empty() {
        return None;
    }
    let url = match reqwest::Url::parse(src) {
        Ok(u) => u.to_string(),
        Err(_) => base.and_then(|b| b.join(src).ok())?.to_string(),
    };
    Some(crate::html::shrink_cdn(&url))
}

/// Splits the leading `> `, indent and bullet off a line's text.
/// Returns (quote depth, indent, bullet, bytes consumed).
fn split_prefix(text: &str) -> (usize, usize, Option<String>, usize) {
    let mut rest = text;
    let mut depth = 0;
    while let Some(r) = rest.strip_prefix("> ") {
        depth += 1;
        rest = r;
    }
    let trimmed = rest.trim_start_matches(' ');
    let indent = rest.len() - trimmed.len();
    rest = trimmed;
    let mut bullet = None;
    if let Some(r) = rest.strip_prefix("• ") {
        bullet = Some("• ".to_string());
        rest = r;
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && digits < 4 && rest[digits..].starts_with(". ") {
            bullet = Some(rest[..digits + 2].to_string());
            rest = &rest[digits + 2..];
        }
    }
    (depth, indent, bullet, text.len() - rest.len())
}

fn style_for(tags: &[Ann], quoted: bool, heading: bool) -> Style {
    let mut style = Style::default().fg(theme::FG1);
    if quoted {
        style = style.fg(theme::FG3).add_modifier(Modifier::ITALIC);
    }
    for t in tags {
        style = match t {
            Ann::Link(_) => style.patch(theme::link()),
            Ann::Emphasis => style.add_modifier(Modifier::ITALIC),
            Ann::Strong => style.fg(theme::FG0).add_modifier(Modifier::BOLD),
            Ann::Strike => style.add_modifier(Modifier::CROSSED_OUT),
            Ann::Code => style.fg(theme::ORANGE).bg(theme::BG1),
            Ann::Pre => style.fg(theme::FG3).bg(theme::BG0_H),
            Ann::Plain | Ann::Image(_) => style,
        };
    }
    if heading {
        style = style.fg(theme::ACCENT).add_modifier(Modifier::BOLD);
    }
    style
}

fn convert_line(line: &TaggedLine<Vec<Ann>>, base: Option<&reqwest::Url>) -> Vec<Block> {
    // The line's styled pieces, with images split out.
    enum Piece {
        Text(String, Vec<Ann>),
        Image(String, String),
    }
    let mut pieces: Vec<Piece> = Vec::new();
    for el in line.iter() {
        let TaggedLineElement::Str(ts) = el else {
            continue;
        };
        if let Some(src) = ts.tag.iter().find_map(|t| match t {
            Ann::Image(s) => Some(s.clone()),
            _ => None,
        }) {
            let alt = ts.s.trim_start_matches(IMAGE).trim().to_string();
            pieces.push(Piece::Image(src, alt));
        } else {
            // html2text strikes text with combining overlays; the style does that here.
            pieces.push(Piece::Text(ts.s.replace('\u{0336}', ""), ts.tag.clone()));
        }
    }

    let plain: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t, _) => t.as_str(),
            Piece::Image(..) => "",
        })
        .collect();
    if plain.trim().is_empty() && !pieces.iter().any(|p| matches!(p, Piece::Image(..))) {
        return vec![Block::Blank];
    }
    let is_pre = pieces
        .iter()
        .any(|p| matches!(p, Piece::Text(_, tags) if tags.contains(&Ann::Pre)));
    let is_table = plain.contains('│') || plain.starts_with('─') || plain.starts_with('┌');
    let heading = plain.contains(HEADING);
    let (depth, indent, bullet, mut skip) = if is_pre {
        let (d, ..) = split_prefix(&plain);
        (d, 0, None, d * 2)
    } else {
        split_prefix(&plain)
    };
    let quoted = depth > 0;

    // Prefix spans.
    let bars = Span::styled("▎ ".repeat(depth), Style::default().fg(theme::BG3));
    let mut prefix = Vec::new();
    let mut cont = Vec::new();
    if depth > 0 {
        prefix.push(bars.clone());
        cont.push(bars);
    }
    if indent > 0 {
        prefix.push(Span::raw(" ".repeat(indent)));
    }
    let bullet_w = bullet.as_ref().map_or(0, |b| b.width());
    if let Some(b) = bullet {
        let style = if b.starts_with('•') {
            Style::default().fg(theme::ACCENT_2)
        } else {
            Style::default().fg(theme::FG3)
        };
        prefix.push(Span::styled(b, style));
    }
    if indent + bullet_w > 0 {
        cont.push(Span::raw(" ".repeat(indent + bullet_w)));
    }

    let wrap = !(is_pre || is_table);
    let mut out = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut flush = |spans: &mut Vec<Span<'static>>, out: &mut Vec<Block>| {
        if spans.iter().any(|s| !s.content.trim().is_empty()) {
            out.push(Block::Para {
                prefix: std::mem::take(&mut prefix),
                cont: cont.clone(),
                spans: std::mem::take(spans),
                wrap,
            });
            // Text after an image on the same line continues the paragraph.
            prefix = cont.clone();
        } else {
            spans.clear();
        }
    };

    for piece in pieces {
        match piece {
            Piece::Image(src, alt) => {
                flush(&mut spans, &mut out);
                match resolve(&src, base) {
                    Some(url) => out.push(Block::Image { url, alt }),
                    None if !alt.is_empty() => out.push(Block::Para {
                        prefix: vec![],
                        cont: vec![],
                        spans: vec![Span::styled(format!("[image: {alt}]"), theme::meta())],
                        wrap: true,
                    }),
                    None => {}
                }
            }
            Piece::Text(mut text, tags) => {
                // Drop the prefix characters already turned into spans.
                if skip > 0 {
                    let mut n = skip.min(text.len());
                    while !text.is_char_boundary(n) {
                        n -= 1;
                    }
                    text = text[n..].to_string();
                    skip -= n;
                }
                text = text.replace(HEADING, "");
                if heading && spans.is_empty() {
                    text = text.trim_start().to_string();
                }
                if text.is_empty() {
                    continue;
                }
                spans.push(Span::styled(text, style_for(&tags, quoted, heading)));
            }
        }
    }
    flush(&mut spans, &mut out);
    out
}

// ----- layout --------------------------------------------------------------

/// What the layout needs to know about an image.
pub enum Slot {
    /// Loaded: its size in cells within the given bounds.
    Ready(Size),
    /// Still loading: reserve this much room.
    Loading(Size),
    /// Failed or images are off.
    Missing,
}

#[derive(Debug, Clone)]
pub enum Placed {
    Text(Line<'static>),
    Image {
        url: String,
        size: Size,
        loading: bool,
    },
}

/// One positioned piece: `y` rows down, `x` columns in.
#[derive(Debug, Clone)]
pub struct Piece {
    pub y: usize,
    pub x: u16,
    pub what: Placed,
}

pub struct Layout {
    pub pieces: Vec<Piece>,
    pub height: usize,
}

pub struct Bounds {
    /// Pane size in cells.
    pub width: u16,
    pub height: u16,
    /// Lead image width, percent of the pane (0: no wrapping, show it inline).
    pub lead_pct: u16,
    /// Tallest image, percent of the pane height.
    pub max_h_pct: u16,
}

/// Lays the article out. `slot(url, max_w, max_h)` reports each image.
pub fn layout(doc: &Doc, b: &Bounds, slot: &mut dyn FnMut(&str, u16, u16) -> Slot) -> Layout {
    let width = b.width.max(10);
    let max_h = ((u32::from(b.height) * u32::from(b.max_h_pct) / 100) as u16).max(4);
    let mut out = Vec::new();

    // The lead image, floated top-left when the pane is wide enough.
    let float_w_max = (u32::from(width) * u32::from(b.lead_pct) / 100) as u16;
    let mut float: Option<(u16, usize)> = None;
    let mut lead_inline = None;
    if let Some((url, alt)) = &doc.lead {
        if b.lead_pct > 0 && width >= 60 && float_w_max >= 12 {
            let (size, loading) = match slot(url, float_w_max, max_h) {
                Slot::Ready(s) => (Some(s), false),
                Slot::Loading(s) => (Some(s), true),
                Slot::Missing => (None, false),
            };
            if let Some(size) = size {
                out.push(Piece {
                    y: 0,
                    x: 0,
                    what: Placed::Image {
                        url: url.clone(),
                        size,
                        loading,
                    },
                });
                float = Some((size.width, usize::from(size.height)));
            }
        } else {
            lead_inline = Some((url.clone(), alt.clone()));
        }
    }
    let col = move |y: usize| -> (u16, u16) {
        match float {
            Some((fw, fh)) if y < fh => (fw + GAP, width.saturating_sub(fw + GAP)),
            _ => (0, width),
        }
    };

    let mut y = 0usize;
    // Header.
    let title_style = Style::default().fg(theme::FG0).add_modifier(Modifier::BOLD);
    let title = [Span::styled(doc.title.clone(), title_style)];
    wrap_into(&mut out, &mut y, &col, &[], &[], &title);
    wrap_into(&mut out, &mut y, &col, &[], &[], &doc.meta);
    if let Some(link) = &doc.link {
        let (x, w) = col(y);
        let line = Line::styled(
            truncate(link, usize::from(w)),
            Style::default().fg(theme::BLUE),
        );
        out.push(Piece {
            y,
            x,
            what: Placed::Text(line),
        });
        y += 1;
    }
    if let Some(note) = &doc.note {
        wrap_into(&mut out, &mut y, &col, &[], &[], &note.spans);
    }
    let (x, w) = col(y);
    out.push(Piece {
        y,
        x,
        what: Placed::Text(Line::styled(
            "─".repeat(usize::from(w)),
            Style::default().fg(theme::BG2),
        )),
    });
    y += 1;

    let mut place_image = |out: &mut Vec<Piece>, y: &mut usize, url: &str, alt: &str| {
        // Images never sit beside the lead image: clear it first.
        if let Some((_, fh)) = float {
            *y = (*y).max(fh + 1);
        }
        let (size, loading) = match slot(url, width, max_h) {
            Slot::Ready(s) => (s, false),
            Slot::Loading(s) => (s, true),
            Slot::Missing => {
                if !alt.is_empty() {
                    out.push(Piece {
                        y: *y,
                        x: 0,
                        what: Placed::Text(Line::styled(
                            truncate(&format!("[image: {alt}]"), usize::from(width)),
                            theme::meta(),
                        )),
                    });
                    *y += 1;
                }
                return;
            }
        };
        out.push(Piece {
            y: *y,
            x: (width - size.width.min(width)) / 2,
            what: Placed::Image {
                url: url.to_string(),
                size,
                loading,
            },
        });
        *y += usize::from(size.height);
        for line in wrap(alt, usize::from(width)) {
            let lw = line.width() as u16;
            out.push(Piece {
                y: *y,
                x: width.saturating_sub(lw) / 2,
                what: Placed::Text(Line::styled(
                    line,
                    theme::meta().add_modifier(Modifier::ITALIC),
                )),
            });
            *y += 1;
        }
    };

    // Body: a blank line after the rule, then the blocks.
    y += 1;
    if let Some((url, alt)) = &lead_inline {
        place_image(&mut out, &mut y, url, alt);
        y += 1;
    }
    for block in &doc.blocks {
        match block {
            Block::Blank => y += 1,
            Block::Image { url, alt } => place_image(&mut out, &mut y, url, alt),
            Block::Para {
                prefix,
                cont,
                spans,
                wrap: true,
            } => wrap_into(&mut out, &mut y, &col, prefix, cont, spans),
            Block::Para {
                prefix,
                spans,
                wrap: false,
                ..
            } => {
                let (x, w) = col(y);
                let mut all = prefix.clone();
                all.extend(spans.iter().cloned());
                out.push(Piece {
                    y,
                    x,
                    what: Placed::Text(clip_line(&Line::from(all), usize::from(w))),
                });
                y += 1;
            }
        }
    }
    let height = y.max(float.map_or(0, |f| f.1));
    Layout {
        pieces: out,
        height,
    }
}

/// A word: runs of non-space text (possibly in several styles), with the
/// style of the space before it (`None` at the start of a paragraph).
struct Word {
    parts: Vec<(String, Style)>,
    width: usize,
    space: Option<Style>,
}

fn words_of(spans: &[Span<'static>]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut cur: Option<Word> = None;
    let mut space: Option<Style> = None;
    for span in spans {
        let mut buf = String::new();
        let add = |buf: &mut String, cur: &mut Option<Word>, space: &mut Option<Style>| {
            if buf.is_empty() {
                return;
            }
            let w = cur.get_or_insert_with(|| Word {
                parts: vec![],
                width: 0,
                space: space.take(),
            });
            w.width += buf.width();
            w.parts.push((std::mem::take(buf), span.style));
        };
        for c in span.content.chars() {
            if c.is_whitespace() {
                add(&mut buf, &mut cur, &mut space);
                if let Some(w) = cur.take() {
                    words.push(w);
                }
                space.get_or_insert(span.style);
            } else {
                buf.push(c);
            }
        }
        add(&mut buf, &mut cur, &mut space);
    }
    if let Some(w) = cur.take() {
        words.push(w);
    }
    words
}

/// Word-wraps styled text, each row as wide as `col(y)` allows.
fn wrap_into(
    out: &mut Vec<Piece>,
    y: &mut usize,
    col: &dyn Fn(usize) -> (u16, u16),
    prefix: &[Span<'static>],
    cont: &[Span<'static>],
    spans: &[Span<'static>],
) {
    let width_of = |s: &[Span<'static>]| s.iter().map(|x| x.width()).sum::<usize>();
    let cont_w = width_of(cont);
    let mut line: Vec<Span<'static>> = prefix.to_vec();
    let mut used = width_of(prefix);
    let mut empty = true;
    let mut emit = |line: &mut Vec<Span<'static>>, y: &mut usize| {
        let (x, _) = col(*y);
        let spans = std::mem::replace(line, cont.to_vec());
        out.push(Piece {
            y: *y,
            x,
            what: Placed::Text(Line::from(spans)),
        });
        *y += 1;
    };
    for word in words_of(spans) {
        let avail = usize::from(col(*y).1).max(1);
        let space = usize::from(!empty && word.space.is_some());
        if !empty && used + space + word.width > avail {
            emit(&mut line, y);
            used = cont_w;
            empty = true;
        }
        if !empty && let Some(st) = word.space {
            // Keep a link's underline across its spaces, not around it.
            let style = if word.parts.first().is_some_and(|p| p.1 == st) {
                st
            } else {
                Style::default()
            };
            line.push(Span::styled(" ", style));
            used += 1;
        }
        for (text, style) in word.parts {
            // A word longer than the row is broken by characters.
            let mut piece = String::new();
            for c in text.chars() {
                let cw = c.width().unwrap_or(0);
                let avail = usize::from(col(*y).1).max(1);
                if used + cw > avail && used > cont_w {
                    if !piece.is_empty() {
                        line.push(Span::styled(std::mem::take(&mut piece), style));
                    }
                    emit(&mut line, y);
                    used = cont_w;
                }
                piece.push(c);
                used += cw;
            }
            if !piece.is_empty() {
                line.push(Span::styled(piece, style));
            }
        }
        empty = false;
    }
    if !empty {
        emit(&mut line, y);
    }
}

fn clip_line(line: &Line<'static>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for s in &line.spans {
        if used >= width {
            break;
        }
        let w = s.width();
        if used + w <= width {
            spans.push(s.clone());
            used += w;
        } else {
            spans.push(Span::styled(truncate(&s.content, width - used), s.style));
            used = width;
        }
    }
    Line::from(spans)
}

/// Plain word wrap.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let need = usize::from(!cur.is_empty()) + word.width();
        if !cur.is_empty() && cur.width() + need > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Cuts `s` to `width` columns, ending with `…` when shortened.
pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(body: &str) -> Item {
        Item {
            id: "x".into(),
            feed_url: "https://example.com/feed".into(),
            title: "A title".into(),
            link: Some("https://example.com/post/1".into()),
            author: None,
            published: None,
            body: body.into(),
            image: None,
        }
    }

    fn doc(body: &str) -> Doc {
        let it = item(body);
        build(&it, "Src", &it.body, None)
    }

    fn lines(l: &Layout) -> Vec<(usize, u16, String)> {
        l.pieces
            .iter()
            .map(|i| {
                let t = match &i.what {
                    Placed::Text(l) => l.spans.iter().map(|s| s.content.as_ref()).collect(),
                    Placed::Image { url, size, .. } => {
                        format!("<IMG {url} {}x{}>", size.width, size.height)
                    }
                };
                (i.y, i.x, t)
            })
            .collect()
    }

    fn bounds(w: u16) -> Bounds {
        Bounds {
            width: w,
            height: 40,
            lead_pct: 40,
            max_h_pct: 40,
        }
    }

    fn ready(_: &str, w: u16, h: u16) -> Slot {
        Slot::Ready(Size::new(w.min(30), h.min(8)))
    }

    #[test]
    fn first_image_becomes_lead_and_text_wraps_beside_it() {
        let d = doc(&format!(
            r#"<p><img src="/lead.jpg"></p><p>{}</p><p><img src="/in.png" alt="cap"></p>"#,
            "word ".repeat(80)
        ));
        assert_eq!(d.lead.as_ref().unwrap().0, "https://example.com/lead.jpg");
        assert_eq!(d.images.len(), 2);
        let l = layout(&d, &bounds(80), &mut ready);
        let ls = lines(&l);
        // Lead at the top-left, 30x8 (40% of 80 = 32 wide max).
        assert_eq!(
            ls[0],
            (0, 0, "<IMG https://example.com/lead.jpg 30x8>".into())
        );
        // Title beside it.
        assert_eq!(ls[1], (0, 32, "A title".into()));
        // Text rows beside the image start at column 32 and fit 48 columns.
        for (y, x, t) in &ls[1..] {
            if *y < 8 && !t.starts_with("<IMG") {
                assert_eq!(*x, 32, "{t}");
                assert!(t.width() <= 48, "{t}");
            }
        }
        // Below the image, full width.
        assert!(
            ls.iter()
                .any(|(y, x, t)| *y >= 8 && *x == 0 && t.starts_with("word") && t.width() > 60),
            "{ls:#?}"
        );
        // The inline image is centered and its caption follows.
        let img = ls
            .iter()
            .position(|(_, _, t)| t.contains("in.png"))
            .unwrap();
        assert_eq!(ls[img].1, 25);
        assert_eq!(ls[img + 1].2, "cap");
    }

    #[test]
    fn narrow_pane_shows_lead_inline() {
        let d = doc(r#"<p><img src="/lead.jpg"></p><p>Text</p>"#);
        let l = layout(&d, &bounds(40), &mut ready);
        let ls = lines(&l);
        let img = ls.iter().find(|(_, _, t)| t.contains("lead.jpg")).unwrap();
        assert!(img.0 > 2, "{ls:?}");
    }

    #[test]
    fn lists_quotes_and_headings() {
        let d = doc(&format!(
            "<h2>Big heading</h2><blockquote>{}</blockquote><ul><li>{}</li></ul><ol><li>one</li></ol><pre>let x = 1;\n  let y = 2;</pre>",
            "quoted words ".repeat(10),
            "item words ".repeat(10)
        ));
        let l = layout(&d, &bounds(40), &mut |_, _, _| Slot::Missing);
        let t: Vec<String> = lines(&l).into_iter().map(|x| x.2).collect();
        let all = t.join("\n");
        assert!(!all.contains(HEADING) && !all.contains("> "), "{all}");
        assert!(t.contains(&"Big heading".to_string()), "{all}");
        let q: Vec<&String> = t.iter().filter(|l| l.starts_with("▎ ")).collect();
        assert!(q.len() >= 3, "{all}");
        assert!(t.iter().any(|l| l.starts_with("• item")), "{all}");
        assert!(t.iter().any(|l| l.starts_with("  item")), "{all}");
        assert!(t.contains(&"1. one".to_string()), "{all}");
        assert!(t.contains(&"  let y = 2;".to_string()), "{all}");
        assert!(t.iter().all(|l| l.width() <= 40), "{all}");
    }

    #[test]
    fn feed_image_is_lead_unless_shown() {
        let mut it = item("<p>Body</p>");
        it.image = Some("https://example.com/t.jpg?w=200".into());
        let d = build(&it, "S", &it.body, None);
        assert_eq!(d.lead.unwrap().0, "https://example.com/t.jpg?w=200");
        let mut it = item(r#"<p>Text first</p><p><img src="https://example.com/t.jpg?w=900"></p>"#);
        it.image = Some("https://example.com/t.jpg?w=200".into());
        let d = build(&it, "S", &it.body, None);
        assert!(d.lead.is_none());
        assert_eq!(d.images.len(), 1);
    }

    #[test]
    fn wrapped_alt_text_is_one_image() {
        let alt = "a long caption ".repeat(10);
        let d = doc(&format!(r#"<p>x</p><p><img src="/c.png" alt="{alt}"></p>"#));
        let imgs: Vec<&String> = d
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { alt, .. } => Some(alt),
                _ => None,
            })
            .collect();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].split_whitespace().count(), 30);
    }

    #[test]
    fn styles_glue_without_breaking_words() {
        let d = doc("<p>Some <b>bold</b>, then <a href='/x'>a link here</a>.</p>");
        let l = layout(&d, &bounds(80), &mut |_, _, _| Slot::Missing);
        let t: Vec<String> = lines(&l).into_iter().map(|x| x.2).collect();
        assert!(
            t.contains(&"Some bold, then a link here.".to_string()),
            "{t:?}"
        );
    }

    #[test]
    fn long_words_break() {
        let d = doc(&format!("<p>{}</p>", "x".repeat(100)));
        let l = layout(&d, &bounds(30), &mut |_, _, _| Slot::Missing);
        assert!(lines(&l).iter().all(|(_, _, t)| t.width() <= 30));
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hi", 6), "hi");
    }
}
