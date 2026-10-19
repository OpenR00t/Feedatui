// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Fetching and parsing feeds (RSS 0.x/1.0/2.0, Atom, JSON Feed).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One article from a feed, in the shape the app stores and shows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub link: Option<String>,
    pub author: Option<String>,
    pub published: Option<DateTime<Utc>>,
    /// HTML (or plain text) body: the full content if the feed has it, else the summary.
    pub body: String,
    /// A lead image from `media:` / enclosure tags, used when the body has none.
    pub image: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub title: Option<String>,
    pub items: Vec<Item>,
}

pub fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("feedatui/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(25))
        .build()
        .expect("HTTP client")
}

pub fn fetch(client: &reqwest::blocking::Client, url: &str) -> Result<Fetched> {
    let resp = client.get(url).send().context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let bytes = resp.bytes().context("reading response")?;
    parse(url, &bytes)
}

pub fn parse(feed_url: &str, bytes: &[u8]) -> Result<Fetched> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(feed_url))
        .build()
        .parse(bytes)
        .context("not a feed this reader understands")?;

    let title = feed.title.map(|t| clean_inline(&t.content));
    let thumbs = media_thumbnails(bytes);
    let items = feed
        .entries
        .into_iter()
        .enumerate()
        .map(|(n, e)| {
            let link = e
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .or(e.links.first())
                .map(|l| l.href.clone());
            let body = e
                .content
                .as_ref()
                .and_then(|c| c.body.clone())
                .filter(|b| !b.trim().is_empty())
                .or_else(|| e.summary.as_ref().map(|s| s.content.clone()))
                .unwrap_or_default();
            let image = e
                .media
                .iter()
                .find_map(|m| {
                    m.thumbnails
                        .first()
                        .map(|t| t.image.uri.clone())
                        .or_else(|| {
                            m.content.iter().find_map(|c| {
                                let is_image = c
                                    .content_type
                                    .as_ref()
                                    .is_some_and(|t| t.to_string().starts_with("image/"));
                                if is_image {
                                    c.url.as_ref().map(|u| u.to_string())
                                } else {
                                    None
                                }
                            })
                        })
                })
                .or_else(|| thumbs.get(n).cloned().flatten());
            let author = e
                .authors
                .iter()
                .filter_map(|p| p.name.clone())
                .map(|n| n.trim().to_string())
                .find(|n| !n.is_empty());
            Item {
                id: format!(
                    "{:016x}",
                    fnv1a(format!("{feed_url}\u{1f}{}", e.id).as_bytes())
                ),
                feed_url: feed_url.to_string(),
                title: e
                    .title
                    .map(|t| clean_inline(&t.content))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(untitled)".to_string()),
                link,
                author,
                published: e.published.or(e.updated),
                body,
                image,
            }
        })
        .collect();
    Ok(Fetched { title, items })
}

/// The first `media:thumbnail` (or image `media:content`) of each item, in
/// order. feed-rs drops a thumbnail that has no `media:content` beside it,
/// which is how many news feeds send their lead image.
fn media_thumbnails(bytes: &[u8]) -> Vec<Option<String>> {
    use quick_xml::events::Event;
    let mut out: Vec<Option<String>> = Vec::new();
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'<') {
        return out;
    }
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut depth_in_item = 0usize;
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(ev) => ev,
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let local = e.local_name();
                let local: &str = local.as_ref();
                let is_start = matches!(ev, Event::Start(_));
                if depth_in_item == 0 && (local == "item" || local == "entry") {
                    out.push(None);
                    if is_start {
                        depth_in_item = 1;
                    }
                } else if depth_in_item > 0 {
                    if is_start {
                        depth_in_item += 1;
                    }
                    let qname = e.name();
                    let qname: &str = qname.as_ref();
                    let is_media = qname.starts_with("media:");
                    let wanted = local == "thumbnail"
                        || (local == "content"
                            && e.attributes().flatten().any(|a| {
                                a.key.as_ref() == "medium" && a.value.as_ref() == "image"
                            }));
                    if is_media
                        && wanted
                        && let Some(slot) = out.last_mut()
                        && slot.is_none()
                        && let Ok(Some(url)) = e.try_get_attribute("url")
                    {
                        *slot = Some(url.value.replace("&amp;", "&"));
                    }
                }
            }
            Event::End(_) if depth_in_item > 0 => depth_in_item -= 1,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Titles sometimes carry markup or entities; flatten them to one plain line.
pub fn clean_inline(s: &str) -> String {
    let text = if s.contains('<') || s.contains('&') {
        html2text::config::with_decorator(html2text::render::TrivialDecorator::new())
            .raw_mode(true)
            .string_from_read(s.as_bytes(), 10_000)
            .unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Small stable hash for ids and cache file names.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/">
<channel><title>Example &amp; Co</title><link>https://example.com/</link>
<item><title>First &lt;b&gt;post&lt;/b&gt;</title><link>https://example.com/1</link>
<guid>1</guid><pubDate>Mon, 19 Oct 2026 10:00:00 GMT</pubDate>
<description>&lt;p&gt;Hello &lt;img src="/a.png"&gt;&lt;/p&gt;</description>
<media:thumbnail url="https://example.com/t.jpg"/></item>
<item><guid>2</guid><description>No title</description></item>
</channel></rss>"#;

    #[test]
    fn parses_rss() {
        let f = parse("https://example.com/feed", RSS.as_bytes()).unwrap();
        assert_eq!(f.title.as_deref(), Some("Example & Co"));
        assert_eq!(f.items.len(), 2);
        let a = &f.items[0];
        assert_eq!(a.title, "First post");
        assert_eq!(a.link.as_deref(), Some("https://example.com/1"));
        assert!(a.body.contains("<img"));
        assert_eq!(a.image.as_deref(), Some("https://example.com/t.jpg"));
        assert!(a.published.is_some());
        assert_eq!(f.items[1].title, "(untitled)");
        assert_ne!(a.id, f.items[1].id);
    }
}
