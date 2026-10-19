// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Turns an article's HTML into styled terminal lines with image blocks.

use html2text::render::{TaggedLine, TaggedLineElement, TextDecorator};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::feed::Item;
use crate::theme;

// Private-use markers the decorator writes so lines can be styled afterwards.
// (Quote prefixes stay ASCII: html2text measures them in bytes.)
const HEADING: char = '\u{E000}';
const IMAGE: char = '\u{E002}';

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

/// A piece of the laid-out article.
#[derive(Debug, Clone)]
pub enum Block {
    Text(Line<'static>),
    Image { url: String, alt: String },
}

#[derive(Debug, Clone)]
pub struct Article {
    pub blocks: Vec<Block>,
    pub images: Vec<String>,
}

/// Lays out an article (header and body) for a column `width` cells wide.
pub fn build(item: &Item, source: &str, width: u16) -> Article {
    let width = usize::from(width.max(10));
    let mut blocks = Vec::new();

    // Header: title, then source · author · date, then the link.
    for line in wrap(&item.title, width) {
        blocks.push(Block::Text(Line::styled(
            line,
            Style::default().fg(theme::FG0).add_modifier(Modifier::BOLD),
        )));
    }
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
    blocks.push(Block::Text(Line::from(meta)));
    if let Some(link) = &item.link {
        blocks.push(Block::Text(Line::styled(
            truncate(link, width),
            Style::default().fg(theme::BLUE),
        )));
    }
    blocks.push(Block::Text(Line::styled(
        "─".repeat(width),
        Style::default().fg(theme::BG2),
    )));

    let html = if looks_like_html(&item.body) {
        item.body.clone()
    } else {
        plain_to_html(&item.body)
    };
    let base = item
        .link
        .as_deref()
        .and_then(|l| reqwest::Url::parse(l).ok());
    let mut body = render_html(&html, width, base.as_ref());

    // Lead image from the feed's media tags when the body has no pictures.
    let has_image = body.iter().any(|b| matches!(b, Block::Image { .. }));
    if !has_image && let Some(img) = &item.image {
        blocks.push(Block::Image {
            url: img.clone(),
            alt: String::new(),
        });
        blocks.push(Block::Text(Line::default()));
    }

    if body
        .iter()
        .all(|b| matches!(b, Block::Text(l) if l.width() == 0))
    {
        body = vec![Block::Text(Line::styled(
            "This feed has no article text. Press o to open it in the browser.",
            theme::meta().add_modifier(Modifier::ITALIC),
        ))];
    }
    blocks.append(&mut body);

    let mut images = Vec::new();
    for b in &blocks {
        if let Block::Image { url, .. } = b
            && !images.contains(url)
        {
            images.push(url.clone());
        }
    }
    Article { blocks, images }
}

fn looks_like_html(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('<') || (t.contains("</") || t.contains("<br") || t.contains("<img"))
}

fn plain_to_html(s: &str) -> String {
    let esc = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let paras: Vec<String> = esc
        .split("\n\n")
        .map(|p| format!("<p>{}</p>", p.trim().replace('\n', "<br>")))
        .collect();
    paras.join("")
}

fn render_html(html: &str, width: usize, base: Option<&reqwest::Url>) -> Vec<Block> {
    let config = || {
        html2text::config::with_decorator(Decorator)
            .empty_img_mode(html2text::config::ImageRenderMode::ShowAlways)
    };
    let lines = config()
        .lines_from_read(html.as_bytes(), width)
        .or_else(|_| {
            config()
                .allow_width_overflow()
                .lines_from_read(html.as_bytes(), width)
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    let mut blank_run = 0;
    for line in lines {
        for block in convert_line(&line, base) {
            // Long alt text wraps over several lines, each tagged with the
            // same image: join them back into one image.
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
            // Collapse runs of empty lines to one.
            let empty = matches!(&block, Block::Text(l) if l.width() == 0);
            if empty {
                blank_run += 1;
                if blank_run > 1 || out.is_empty() {
                    continue;
                }
            } else {
                blank_run = 0;
            }
            out.push(block);
        }
    }
    while matches!(out.last(), Some(Block::Text(l)) if l.width() == 0) {
        out.pop();
    }
    out
}

fn resolve(src: &str, base: Option<&reqwest::Url>) -> Option<String> {
    if src.starts_with("data:") {
        return None;
    }
    match reqwest::Url::parse(src) {
        Ok(u) => Some(u.to_string()),
        Err(_) => base.and_then(|b| b.join(src).ok()).map(|u| u.to_string()),
    }
}

fn convert_line(line: &TaggedLine<Vec<Ann>>, base: Option<&reqwest::Url>) -> Vec<Block> {
    let mut out = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut heading = false;
    let mut quoted = false;

    let flush = |spans: &mut Vec<Span<'static>>, out: &mut Vec<Block>, force: bool| {
        let blank = spans.iter().all(|s| s.content.trim().is_empty());
        if force || !blank {
            out.push(Block::Text(Line::from(std::mem::take(spans))));
        } else {
            spans.clear();
        }
    };

    for el in line.iter() {
        let TaggedLineElement::Str(ts) = el else {
            continue;
        };
        let tags = &ts.tag;
        if let Some(src) = tags.iter().find_map(|t| match t {
            Ann::Image(s) => Some(s.clone()),
            _ => None,
        }) {
            flush(&mut spans, &mut out, false);
            let alt = ts.s.trim_start_matches(IMAGE).trim().to_string();
            match resolve(&src, base) {
                Some(url) => out.push(Block::Image { url, alt }),
                None if !alt.is_empty() => out.push(Block::Text(Line::styled(
                    format!("[image: {alt}]"),
                    theme::meta(),
                ))),
                None => {}
            }
            continue;
        }

        // html2text strikes text with combining overlays; the style does that here.
        let mut text = ts.s.replace('\u{0336}', "");
        if text.contains(HEADING) {
            heading = true;
            text = text.replace(HEADING, "").trim_start().to_string();
            if text.is_empty() {
                continue;
            }
        }
        if spans.is_empty() {
            let depth = quote_depth(&text);
            if depth > 0 {
                quoted = true;
                spans.push(Span::styled(
                    "▎ ".repeat(depth),
                    Style::default().fg(theme::BG3),
                ));
                text = text[depth * 2..].to_string();
                if text.is_empty() {
                    continue;
                }
            }
        }
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
        if text == "• " {
            spans.push(Span::styled(text, Style::default().fg(theme::ACCENT_2)));
        } else {
            spans.push(Span::styled(text, style));
        }
    }
    let had_image = out.iter().any(|b| matches!(b, Block::Image { .. }));
    flush(&mut spans, &mut out, !had_image);
    out
}

/// How many `> ` prefixes html2text put before a quoted line.
fn quote_depth(s: &str) -> usize {
    s.as_bytes().chunks(2).take_while(|c| *c == b"> ").count()
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let need = if cur.is_empty() { 0 } else { 1 } + word.width();
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
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
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

    fn text_of(a: &Article) -> Vec<String> {
        a.blocks
            .iter()
            .map(|b| match b {
                Block::Text(l) => l.spans.iter().map(|s| s.content.as_ref()).collect(),
                Block::Image { url, .. } => format!("<IMG {url}>"),
            })
            .collect()
    }

    #[test]
    fn images_become_blocks_with_resolved_urls() {
        let a = build(
            &item(
                r#"<p>Before</p><p><img src="/pic.png"></p><p>After <img src="https://x.org/i.jpg" alt="cat"> end</p>"#,
            ),
            "Src",
            60,
        );
        let t = text_of(&a);
        assert!(
            t.contains(&"<IMG https://example.com/pic.png>".to_string()),
            "{t:?}"
        );
        assert!(
            t.contains(&"<IMG https://x.org/i.jpg>".to_string()),
            "{t:?}"
        );
        assert_eq!(a.images.len(), 2);
        assert!(t.iter().any(|l| l.contains("Before")));
        assert!(t.iter().any(|l| l.contains("end")));
    }

    #[test]
    fn headings_and_quotes_lose_markers() {
        let a = build(
            &item(
                "<h2>Big heading here</h2><blockquote>quoted words</blockquote><ul><li>one</li></ul>",
            ),
            "Src",
            60,
        );
        let t = text_of(&a).join("\n");
        assert!(!t.contains(HEADING) && !t.contains("> "), "{t}");
        assert!(t.contains("Big heading here"));
        assert!(t.contains("▎ quoted words"), "{t}");
        assert!(t.contains("• one"), "{t}");
    }

    #[test]
    fn plain_text_and_lead_image() {
        let mut it = item("line one\n\nline two");
        it.image = Some("https://example.com/lead.jpg".into());
        let a = build(&it, "Src", 40);
        let t = text_of(&a);
        assert!(t.contains(&"<IMG https://example.com/lead.jpg>".to_string()));
        assert!(t.contains(&"line one".to_string()));
        assert!(t.contains(&"line two".to_string()));
    }

    #[test]
    fn wrapped_alt_text_is_one_image() {
        let alt = "a long caption ".repeat(10);
        let a = build(
            &item(&format!(r#"<p><img src="/c.png" alt="{alt}"></p>"#)),
            "Src",
            30,
        );
        assert_eq!(a.images.len(), 1);
        let imgs: Vec<&String> = a
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { alt, .. } => Some(alt),
                _ => None,
            })
            .collect();
        assert_eq!(imgs.len(), 1, "{:?}", text_of(&a));
        assert_eq!(imgs[0].split_whitespace().count(), 30);
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hi", 6), "hi");
    }
}
