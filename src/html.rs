// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Cleans article HTML before it is laid out: picks a sensibly sized image
//! from `srcset`, finds lazy-loaded image sources and drops images that are
//! not pictures (tracking pixels, emoji, duplicates in `<noscript>`).

use dom_query::Document;

/// Images are downloaded at about this width when the page offers sizes.
const WANTED_WIDTH: u32 = 1000;

pub fn clean(html: &str) -> String {
    let doc = Document::from(html);

    doc.select("noscript, script, style, iframe, svg, form, button")
        .remove();

    // <picture>: give the inner <img> a source if it lacks one.
    for pic in doc.select("picture").nodes() {
        let img = pic.find(&["img"]).into_iter().next();
        let source = pic
            .find(&["source"])
            .into_iter()
            .find_map(|s| s.attr("srcset").or_else(|| s.attr("data-srcset")));
        if let (Some(img), Some(set)) = (img, source)
            && img.attr("srcset").is_none()
        {
            img.set_attr("srcset", &set);
        }
    }

    for img in doc.select("img").nodes() {
        // Emoji and smilies drawn as images: keep their text.
        let class = img.attr("class").map(|c| c.to_string()).unwrap_or_default();
        let src = img.attr("src").map(|s| s.to_string()).unwrap_or_default();
        if class.contains("emoji") || class.contains("smiley") || src.contains("/emoji/") {
            let alt = img.attr("alt").map(|a| a.to_string()).unwrap_or_default();
            img.replace_with_html(escape(&alt));
            continue;
        }
        // Width/height attributes of 1–2 px: tracking pixels.
        let tiny = |a: &str| {
            img.attr(a)
                .and_then(|v| v.trim_end_matches("px").parse::<u32>().ok())
                .is_some_and(|v| v <= 2)
        };
        if tiny("width") || tiny("height") {
            img.remove_from_parent();
            continue;
        }

        let srcset = img
            .attr("srcset")
            .or_else(|| img.attr("data-srcset"))
            .map(|s| s.to_string());
        let lazy = ["data-src", "data-lazy-src", "data-original", "data-url"]
            .iter()
            .find_map(|a| img.attr(a))
            .map(|s| s.to_string());
        let placeholder = src.is_empty() || src.starts_with("data:");
        let chosen = srcset
            .as_deref()
            .and_then(pick_from_srcset)
            .or(if placeholder { lazy } else { None });
        if let Some(url) = chosen {
            img.set_attr("src", &url);
        }
    }
    doc.select("body").html().to_string()
}

/// Picks the smallest candidate at least [`WANTED_WIDTH`] wide, else the
/// largest. Density descriptors (`2x`) count as 500 px per x.
pub fn pick_from_srcset(set: &str) -> Option<String> {
    let mut cands: Vec<(u32, String)> = Vec::new();
    for part in split_srcset(set) {
        let mut it = part.split_whitespace();
        let Some(url) = it.next() else { continue };
        let w = match it.next() {
            Some(d) if d.ends_with('w') => d[..d.len() - 1].parse().unwrap_or(0),
            Some(d) if d.ends_with('x') => {
                (d[..d.len() - 1].parse::<f32>().unwrap_or(1.0) * 500.0) as u32
            }
            _ => 500,
        };
        cands.push((w, url.to_string()));
    }
    cands.sort_by_key(|c| c.0);
    cands
        .iter()
        .find(|c| c.0 >= WANTED_WIDTH)
        .or(cands.last())
        .map(|c| c.1.clone())
}

/// Splits a srcset on the commas between candidates (URLs may contain commas,
/// e.g. image CDNs, so a comma only splits when followed by whitespace).
fn split_srcset(set: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = set.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b',' && bytes.get(i + 1).is_none_or(|b| b.is_ascii_whitespace()) {
            out.push(set[start..i].trim());
            start = i + 1;
        }
    }
    out.push(set[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Plain words in an HTML fragment.
pub fn word_count(html: &str) -> usize {
    let mut words = 0;
    let mut in_tag = false;
    let mut in_word = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                in_word = false;
            }
            '>' => in_tag = false,
            _ if in_tag => {}
            c if c.is_alphanumeric() => {
                if !in_word {
                    words += 1;
                    in_word = true;
                }
            }
            _ => in_word = false,
        }
    }
    words
}

/// Whether a feed's article text looks like a teaser rather than the article.
pub fn looks_truncated(html: &str) -> bool {
    if word_count(html) < 250 {
        return true;
    }
    let tail: String = html
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    [
        "read more",
        "continue reading",
        "read the full",
        "[…]",
        "[...]",
        "&hellip;]",
        "keep reading",
    ]
    .iter()
    .any(|m| tail.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcset_prefers_about_1000px() {
        let set = "a.jpg 320w, b.jpg 768w, c.jpg 1024w, d.jpg 2048w";
        assert_eq!(pick_from_srcset(set).as_deref(), Some("c.jpg"));
        assert_eq!(
            pick_from_srcset("a.jpg 300w, b.jpg 600w").as_deref(),
            Some("b.jpg")
        );
        assert_eq!(
            pick_from_srcset("x.jpg 1x, y.jpg 2x").as_deref(),
            Some("y.jpg")
        );
        let cdn =
            "https://i.example/w_400,q_80/p.jpg 400w, https://i.example/w_1200,q_80/p.jpg 1200w";
        assert_eq!(
            pick_from_srcset(cdn).as_deref(),
            Some("https://i.example/w_1200,q_80/p.jpg")
        );
    }

    #[test]
    fn clean_fixes_images() {
        let out = clean(
            r#"<p>Hi <img class="wp-smiley" src="/s.png" alt=":)"></p>
            <img src="data:image/gif;base64,AAA" data-src="/lazy.jpg">
            <img src="/big.jpg" srcset="/s.jpg 400w, /m.jpg 1000w, /l.jpg 2000w">
            <img src="/pixel.gif" width="1" height="1">
            <noscript><img src="/dup.jpg"></noscript>"#,
        );
        assert!(out.contains(":)") && !out.contains("s.png"), "{out}");
        assert!(out.contains(r#"src="/lazy.jpg""#), "{out}");
        assert!(out.contains(r#"src="/m.jpg""#), "{out}");
        assert!(
            !out.contains("pixel.gif") && !out.contains("dup.jpg"),
            "{out}"
        );
    }

    #[test]
    fn truncation_heuristic() {
        assert!(looks_truncated("<p>Short teaser.</p>"));
        let long = "<p>".to_string() + &"word ".repeat(400) + "</p>";
        assert!(!looks_truncated(&long));
        assert!(looks_truncated(&(long.clone() + "<a href=x>Read more</a>")));
    }
}
