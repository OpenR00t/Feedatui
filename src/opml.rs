// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! OPML import and export. Imports are merged into `config.toml` with its
//! comments and layout kept; folders become groups.

use std::path::Path;

use anyhow::{Context, Result, bail};
use quick_xml::events::Event;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use crate::config::Config;

/// Group name for feeds that sit outside any OPML folder.
const LOOSE: &str = "Imported";

#[derive(Debug, PartialEq)]
pub struct OpmlFeed {
    pub group: String,
    pub name: Option<String>,
    pub url: String,
}

/// Reads the feeds out of an OPML document, each with its folder's name.
pub fn parse(text: &str) -> Result<Vec<OpmlFeed>> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut feeds = Vec::new();
    // Open <outline> folders (None for an outline that is itself a feed).
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut saw_opml = false;
    loop {
        let ev = reader.read_event().context("not valid OPML")?;
        let (e, is_start) = match &ev {
            Event::Eof => break,
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                if e.local_name().as_ref() == "outline" {
                    stack.pop();
                }
                continue;
            }
            _ => continue,
        };
        let name = e.local_name();
        let name: &str = name.as_ref();
        if name == "opml" {
            saw_opml = true;
            continue;
        }
        if name != "outline" {
            continue;
        }
        let mut url = None;
        let mut title = None;
        let mut text_attr = None;
        for a in e.attributes().flatten() {
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|c| c.trim().to_string())
                .unwrap_or_default();
            match a.key.local_name().as_ref() {
                "xmlUrl" => url = Some(v),
                "title" => title = Some(v),
                "text" => text_attr = Some(v),
                _ => {}
            }
        }
        let label = title
            .filter(|t| !t.is_empty())
            .or(text_attr.filter(|t| !t.is_empty()));
        match url.filter(|u| !u.is_empty()) {
            Some(url) => {
                let group = stack
                    .iter()
                    .rev()
                    .find_map(|g| g.clone())
                    .unwrap_or_else(|| LOOSE.to_string());
                feeds.push(OpmlFeed {
                    group,
                    name: label,
                    url,
                });
                if is_start {
                    stack.push(None);
                }
            }
            None => {
                if is_start {
                    stack.push(Some(label.unwrap_or_else(|| LOOSE.to_string())));
                }
            }
        }
    }
    if !saw_opml {
        bail!("not an OPML file");
    }
    Ok(feeds)
}

pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
    pub new_groups: Vec<String>,
}

/// Merges `feeds` into the config text: into the group of the same name
/// (ignoring case) or a new group at the end. Feeds already in that group
/// are skipped.
pub fn merge_into(config_text: &str, feeds: &[OpmlFeed]) -> Result<(String, ImportReport)> {
    let mut doc: DocumentMut = config_text
        .parse()
        .context("config.toml is not valid TOML")?;
    if !doc.contains_key("group") {
        doc.insert("group", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let groups = doc["group"]
        .as_array_of_tables_mut()
        .context("`group` in config.toml should be [[group]] tables")?;
    let mut report = ImportReport {
        added: 0,
        skipped: 0,
        new_groups: Vec::new(),
    };
    for f in feeds {
        let idx = groups.iter().position(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(&f.group))
        });
        let table = match idx {
            Some(i) => groups.get_mut(i).expect("index from position"),
            None => {
                let mut t = Table::new();
                t.insert("name", toml_edit::value(f.group.clone()));
                groups.push(t);
                report.new_groups.push(f.group.clone());
                groups.get_mut(groups.len() - 1).expect("just pushed")
            }
        };
        if !table.contains_key("feeds") {
            table.insert("feeds", Item::Value(Value::Array(Array::new())));
        }
        let arr = table["feeds"]
            .as_array_mut()
            .context("a group's `feeds` should be an array")?;
        let exists = arr.iter().any(|v| {
            v.as_inline_table()
                .and_then(|t| t.get("url"))
                .and_then(|u| u.as_str())
                == Some(f.url.as_str())
        });
        if exists {
            report.skipped += 1;
            continue;
        }
        let mut entry = InlineTable::new();
        if let Some(n) = &f.name {
            entry.insert("name", n.as_str().into());
        }
        entry.insert("url", f.url.as_str().into());
        let mut v = Value::InlineTable(entry);
        v.decor_mut().set_prefix("\n  ");
        arr.push_formatted(v);
        arr.set_trailing("\n");
        arr.set_trailing_comma(true);
        report.added += 1;
    }
    let text = doc.to_string();
    // Make sure the result still loads before it is written.
    Config::parse(&text).context("the merged config did not load")?;
    Ok((text, report))
}

/// `feedatui import <file>`.
pub fn import(opml_path: &Path, config_path: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(opml_path)
        .with_context(|| format!("reading {}", opml_path.display()))?;
    let feeds = parse(&text).with_context(|| format!("in {}", opml_path.display()))?;
    if feeds.is_empty() {
        bail!("{} has no feeds in it", opml_path.display());
    }
    Config::ensure(config_path)?;
    let config_text = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let (merged, report) = merge_into(&config_text, &feeds)?;
    if report.added > 0 {
        let backup = config_path.with_extension("toml.bak");
        std::fs::copy(config_path, &backup)
            .with_context(|| format!("backing up to {}", backup.display()))?;
        std::fs::write(config_path, merged)
            .with_context(|| format!("writing {}", config_path.display()))?;
    }
    Ok(report)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `feedatui export`: every group as an OPML folder.
pub fn export(cfg: &Config) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>Feedatui feeds</title></head>\n  <body>\n",
    );
    for g in &cfg.groups {
        out.push_str(&format!(
            "    <outline text=\"{0}\" title=\"{0}\">\n",
            esc(&g.name)
        ));
        for f in &g.feeds {
            let name = f.name.clone().unwrap_or_else(|| f.url.clone());
            out.push_str(&format!(
                "      <outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n",
                esc(&name),
                esc(&f.url)
            ));
        }
        out.push_str("    </outline>\n");
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPML: &str = r#"<?xml version="1.0"?>
<opml version="1.0"><head><title>x</title></head><body>
  <outline text="Tech">
    <outline type="rss" text="Rust Blog" xmlUrl="https://blog.rust-lang.org/feed.xml"/>
    <outline type="rss" text="New &amp; Shiny" xmlUrl="https://new.example/feed"/>
  </outline>
  <outline text="Loose" title="Loose feed" xmlUrl="https://loose.example/rss"/>
  <outline title="Nested"><outline text="Inner"><outline text="Deep" xmlUrl="https://deep.example/"/></outline></outline>
</body></opml>"#;

    #[test]
    fn parses_folders() {
        let f = parse(OPML).unwrap();
        assert_eq!(f.len(), 4);
        assert_eq!(f[0].group, "Tech");
        assert_eq!(f[1].name.as_deref(), Some("New & Shiny"));
        assert_eq!(f[2].group, LOOSE);
        assert_eq!(f[2].name.as_deref(), Some("Loose feed"));
        assert_eq!(f[3].group, "Inner");
    }

    #[test]
    fn merge_keeps_comments_and_skips_duplicates() {
        let cfg = "# my comment\nrefresh_minutes = 5\n\n[[group]]\nname = \"tech\"\nfeeds = [\n  { url = \"https://blog.rust-lang.org/feed.xml\" },\n]\n";
        let (text, r) = merge_into(cfg, &parse(OPML).unwrap()).unwrap();
        assert!(text.starts_with("# my comment"), "{text}");
        assert_eq!(r.added, 3);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.new_groups, vec![LOOSE.to_string(), "Inner".to_string()]);
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.groups[0].feeds.len(), 2);
        assert_eq!(c.groups[0].feeds[1].name.as_deref(), Some("New & Shiny"));
        assert_eq!(c.groups.len(), 3);
        // Importing again changes nothing.
        let (_, r2) = merge_into(&text, &parse(OPML).unwrap()).unwrap();
        assert_eq!(r2.added, 0);
    }

    #[test]
    fn export_round_trips() {
        let cfg = Config::parse(crate::config::DEFAULT_CONFIG).unwrap();
        let back = parse(&export(&cfg)).unwrap();
        let n: usize = cfg.groups.iter().map(|g| g.feeds.len()).sum();
        assert_eq!(back.len(), n);
        assert_eq!(back[0].group, "Tech");
    }
}
