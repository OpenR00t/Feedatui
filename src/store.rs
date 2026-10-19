// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Saved state: articles seen so far (so the list shows instantly and keeps
//! items a feed has dropped), feed titles and what has been read.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::feed::Item;

/// Articles kept per feed.
const KEEP_PER_FEED: usize = 300;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub items: HashMap<String, Vec<Item>>,
    #[serde(default)]
    pub feed_titles: HashMap<String, String>,
    #[serde(default)]
    pub read: HashSet<String>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        // Forget read marks for articles no longer kept.
        let known: HashSet<&str> = self
            .items
            .values()
            .flatten()
            .map(|i| i.id.as_str())
            .collect();
        let read: HashSet<String> = self
            .read
            .iter()
            .filter(|id| known.contains(id.as_str()))
            .cloned()
            .collect();
        self.read = read;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Merges a fresh fetch into what is stored. New copies replace old ones
    /// (articles get edited); articles the feed dropped are kept.
    pub fn merge(&mut self, feed_url: &str, fresh: Vec<Item>) {
        let list = self.items.entry(feed_url.to_string()).or_default();
        let mut by_id: HashMap<String, Item> = list.drain(..).map(|i| (i.id.clone(), i)).collect();
        // Feeds without dates keep their own order: newest first as published.
        let now = chrono::Utc::now();
        for (n, mut item) in fresh.into_iter().enumerate() {
            if item.published.is_none() {
                item.published = by_id
                    .get(&item.id)
                    .and_then(|old| old.published)
                    .or_else(|| Some(now - chrono::Duration::seconds(n as i64)));
            }
            by_id.insert(item.id.clone(), item);
        }
        let mut merged: Vec<Item> = by_id.into_values().collect();
        merged.sort_by_key(|i| std::cmp::Reverse(i.published));
        merged.truncate(KEEP_PER_FEED);
        *list = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, secs: i64) -> Item {
        Item {
            id: id.into(),
            feed_url: "f".into(),
            title: id.into(),
            link: None,
            author: None,
            published: chrono::DateTime::from_timestamp(secs, 0),
            body: String::new(),
            image: None,
        }
    }

    #[test]
    fn merge_keeps_dropped_and_sorts() {
        let mut s = Store::default();
        s.merge("f", vec![item("a", 10), item("b", 20)]);
        s.merge("f", vec![item("c", 30), item("b", 20)]);
        let ids: Vec<&str> = s.items["f"].iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
    }

    #[test]
    fn save_prunes_unknown_read_marks() {
        let dir = std::env::temp_dir().join(format!("feedatui-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut s = Store::default();
        s.merge("f", vec![item("a", 10)]);
        s.read.insert("a".into());
        s.read.insert("gone".into());
        s.save(&path).unwrap();
        let back = Store::load(&path);
        assert!(back.read.contains("a"));
        assert!(!back.read.contains("gone"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
