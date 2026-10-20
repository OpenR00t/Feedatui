// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Pages built with the Next.js app router often send the article not as
//! HTML but as a React Server Components payload in
//! `self.__next_f.push([1, "…"])` scripts, rendered in the browser. This
//! rebuilds plain HTML from that payload so Readability can read it.

use std::collections::{HashMap, HashSet};

use dom_query::Document;
use serde_json::Value;

/// Rebuilds the page from its RSC payload. `None` if the page has none.
pub fn rebuild(page: &Document) -> Option<String> {
    let mut flight = String::new();
    for script in page.select("script").nodes() {
        let text = script.text();
        let Some(rest) = text.trim().strip_prefix("self.__next_f.push(") else {
            continue;
        };
        let Some(json) = rest.trim_end_matches(';').strip_suffix(')') else {
            continue;
        };
        if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(json)
            && let Some(Value::String(s)) = a.get(1)
        {
            flight.push_str(s);
        }
    }
    if flight.is_empty() {
        return None;
    }
    let rows = parse_rows(&flight);
    Some(render_rows(&rows))
}

enum Row {
    Json(Value),
    Text(String),
}

/// Splits the flight data into `id:payload` rows. Text rows (`id:T<hex
/// byte length>,<text>`) can contain newlines; other rows end at one.
fn parse_rows(data: &str) -> HashMap<String, Row> {
    let mut rows = HashMap::new();
    let bytes = data.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(colon) = data[i..].find(':').map(|c| i + c) else {
            break;
        };
        let id = data[i..colon].trim().to_string();
        let rest = colon + 1;
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) || id.is_empty() {
            // Not a row start: skip to the next line.
            match data[i..].find('\n') {
                Some(n) => i += n + 1,
                None => break,
            }
            continue;
        }
        if bytes.get(rest) == Some(&b'T')
            && let Some(comma) = data[rest..].find(',').map(|c| rest + c)
            && let Ok(len) = usize::from_str_radix(&data[rest + 1..comma], 16)
        {
            let start = comma + 1;
            let mut end = (start + len).min(data.len());
            while !data.is_char_boundary(end) {
                end -= 1;
            }
            rows.insert(id, Row::Text(data[start..end].to_string()));
            i = end;
            continue;
        }
        let end = data[rest..].find('\n').map_or(data.len(), |n| rest + n);
        if let Ok(v) = serde_json::from_str::<Value>(&data[rest..end]) {
            rows.insert(id, Row::Json(v));
        }
        i = end + 1;
    }
    rows
}

/// Elements copied over as HTML; other tags are skipped with their content
/// kept (or dropped, for `SKIP`).
const KEEP: &[&str] = &[
    "html",
    "body",
    "main",
    "article",
    "section",
    "header",
    "footer",
    "nav",
    "aside",
    "div",
    "span",
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "img",
    "picture",
    "source",
    "figure",
    "figcaption",
    "ul",
    "ol",
    "li",
    "blockquote",
    "strong",
    "b",
    "em",
    "i",
    "u",
    "s",
    "del",
    "code",
    "pre",
    "br",
    "hr",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "sup",
    "sub",
];
const SKIP: &[&str] = &[
    "script", "style", "svg", "button", "form", "input", "select", "textarea", "noscript",
    "iframe", "template", "head", "meta", "link", "title",
];
const ATTRS: &[(&str, &str)] = &[
    ("className", "class"),
    ("id", "id"),
    ("src", "src"),
    ("srcSet", "srcset"),
    ("alt", "alt"),
    ("href", "href"),
];
/// String props of components that commonly carry the article text.
const TEXT_PROPS: &[&str] = &["text", "content", "body", "html", "markdown"];

struct Renderer<'a> {
    rows: &'a HashMap<String, Row>,
    seen: HashSet<String>,
    out: String,
}

fn render_rows(rows: &HashMap<String, Row>) -> String {
    // Row 0 is the page root; anything it never reaches follows in id order.
    let mut ids: Vec<&String> = rows.keys().collect();
    ids.sort_by_key(|id| {
        (
            id.as_str() != "0",
            u64::from_str_radix(id, 16).unwrap_or(u64::MAX),
        )
    });
    let mut r = Renderer {
        rows,
        seen: HashSet::new(),
        out: String::from("<html><body>"),
    };
    for id in ids {
        r.row(id);
    }
    r.out.push_str("</body></html>");
    r.out
}

/// `$1a`, `$@1a` (promise) and `$L1a` (lazy) point at another row. As an
/// element type, `$L1a` names a component module instead; those rows are
/// not JSON and never rendered.
fn ref_id(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('$')?;
    let rest = rest
        .strip_prefix('@')
        .or_else(|| rest.strip_prefix('L'))
        .unwrap_or(rest);
    (!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit())).then_some(rest)
}

impl Renderer<'_> {
    fn row(&mut self, id: &str) {
        if !self.seen.insert(id.to_string()) {
            return;
        }
        match self.rows.get(id) {
            Some(Row::Json(v)) => self.node(v),
            Some(Row::Text(t)) => self.text_prop(t),
            None => {}
        }
    }

    fn node(&mut self, v: &Value) {
        match v {
            Value::String(s) => {
                if let Some(id) = ref_id(s) {
                    self.row(id);
                } else if s.starts_with('$') && !s.starts_with("$$") {
                } else {
                    let s = s
                        .strip_prefix('$')
                        .filter(|r| r.starts_with('$'))
                        .unwrap_or(s);
                    self.out.push_str(&escape(s));
                }
            }
            Value::Number(n) => self.out.push_str(&n.to_string()),
            Value::Array(a) => {
                if a.first().and_then(Value::as_str) == Some("$") && a.len() >= 4 {
                    self.element(a.get(1).and_then(Value::as_str).unwrap_or(""), &a[3]);
                } else {
                    a.iter().for_each(|x| self.node(x));
                }
            }
            _ => {}
        }
    }

    fn element(&mut self, kind: &str, props: &Value) {
        let Value::Object(props) = props else { return };
        if SKIP.contains(&kind) {
            return;
        }
        let keep = KEEP.contains(&kind);
        if keep {
            self.out.push('<');
            self.out.push_str(kind);
            for (from, to) in ATTRS {
                if let Some(Value::String(v)) = props.get(*from)
                    && !v.starts_with('$')
                {
                    self.out.push_str(&format!(" {to}=\"{}\"", escape(v)));
                }
            }
            self.out.push('>');
            if matches!(kind, "img" | "source" | "br" | "hr") {
                return;
            }
        } else {
            // A component: its text-like props are often the content, and
            // elements can arrive in any prop (`fullContent`, `body`…).
            for key in TEXT_PROPS {
                if let Some(Value::String(t)) = props.get(*key) {
                    self.text_prop(t);
                }
            }
            for (key, value) in props {
                if key != "children" && is_element_tree(value) {
                    self.node(value);
                }
            }
        }
        if let Some(Value::String(html)) = props
            .get("dangerouslySetInnerHTML")
            .and_then(|d| d.get("__html"))
        {
            self.out.push_str(html);
        }
        if let Some(children) = props.get("children") {
            self.node(children);
        }
        if keep {
            self.out.push_str(&format!("</{kind}>"));
        }
    }

    /// A text prop: HTML as is, otherwise Markdown.
    fn text_prop(&mut self, t: &str) {
        if t.trim().is_empty() || t.starts_with('$') {
            return;
        }
        let looks_html = t.trim_start().starts_with('<') && t.contains("</");
        if looks_html {
            self.out.push_str(t);
        } else if t.contains(' ') {
            let parser = pulldown_cmark::Parser::new(t);
            let mut html = String::new();
            pulldown_cmark::html::push_html(&mut html, parser);
            self.out.push_str("<div>");
            self.out.push_str(&html);
            self.out.push_str("</div>");
        }
    }
}

/// A React element, or a list holding one.
fn is_element_tree(v: &Value) -> bool {
    match v {
        Value::Array(a) => {
            (a.first().and_then(Value::as_str) == Some("$") && a.len() >= 4)
                || a.iter().any(is_element_tree)
        }
        _ => false,
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The `articleBody` of a JSON-LD article, as paragraphs.
pub fn json_ld_body(page: &Document) -> Option<String> {
    fn find(v: &Value) -> Option<String> {
        match v {
            Value::Object(o) => {
                if let Some(Value::String(b)) = o.get("articleBody") {
                    return Some(b.clone());
                }
                o.values().find_map(find)
            }
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    let body = page
        .select(r#"script[type="application/ld+json"]"#)
        .nodes()
        .iter()
        .filter_map(|s| serde_json::from_str::<Value>(&s.text()).ok())
        .find_map(|v| find(&v))?;
    let paras: String = body
        .split('\n')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("<p>{}</p>", escape(p)))
        .collect();
    (!paras.is_empty()).then_some(paras)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_with(flight: &str) -> Document {
        let mut html = String::from("<html><body><div id=app>Loading</div>");
        // Split into several pushes, as Next.js does.
        for chunk in flight.as_bytes().chunks(60) {
            let s = String::from_utf8_lossy(chunk);
            html.push_str(&format!(
                "<script>self.__next_f.push([1,{}])</script>",
                serde_json::to_string(&s).unwrap()
            ));
        }
        html.push_str("</body></html>");
        Document::from(html)
    }

    #[test]
    fn rebuilds_elements_markdown_and_refs() {
        let flight = concat!(
            "1:I[\"chunk.js\",[],\"Comp\"]\n",
            "2:T1a,## Heading\n\nText row body\n",
            "0:[\"$\",\"main\",null,{\"className\":\"content\",\"children\":[",
            "[\"$\",\"$L1\",null,{\"text\":\"### Part one\\n\\nSome *markdown* text here.\"}],",
            "[\"$\",\"img\",null,{\"src\":\"https://x.example/a.jpg\",\"alt\":\"A pic\"}],",
            "[\"$\",\"p\",null,{\"children\":\"Plain & simple\"}],",
            "[\"$\",\"svg\",null,{\"children\":\"no\"}],",
            "\"$2\"",
            "]}]\n"
        );
        let html = rebuild(&page_with(flight)).unwrap();
        assert!(html.contains("<main class=\"content\">"), "{html}");
        assert!(html.contains("<h3>Part one</h3>"), "{html}");
        assert!(html.contains("<em>markdown</em>"), "{html}");
        assert!(html.contains("src=\"https://x.example/a.jpg\""), "{html}");
        assert!(html.contains("Plain &amp; simple"), "{html}");
        assert!(html.contains("<h2>Heading</h2>"), "{html}");
        assert!(!html.contains(">no<"), "{html}");
        // The text row is rendered once, where it is referenced.
        assert_eq!(html.matches("Heading").count(), 1, "{html}");
    }

    #[test]
    fn no_payload_is_none() {
        assert!(rebuild(&Document::from("<html><body><p>x</p></body></html>")).is_none());
    }

    #[test]
    fn json_ld_article_body() {
        let d = Document::from(
            r#"<html><head><script type="application/ld+json">{"@type":"NewsArticle","articleBody":"First para.\nSecond <para>."}</script></head></html>"#,
        );
        assert_eq!(
            json_ld_body(&d).unwrap(),
            "<p>First para.</p><p>Second &lt;para&gt;.</p>"
        );
    }
}
