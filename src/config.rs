// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! `config.toml`: feed groups and settings, plus where files live.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = r#"# Feedatui configuration
#
# Feeds are organised in groups. Press `b` in the app to pick a group
# (or a single feed inside one); `gt` / `gT` step through the groups.
# `name` is optional: without it the feed's own title is used.
# `feedatui import <file.opml>` adds feeds from another reader.

# Minutes between automatic refreshes (0 turns it off).
refresh_minutes = 30

# Full-article fetch: many feeds only carry a teaser, so Feedatui can fetch
# the article's web page and pull the article out of it.
#   "auto"   fetch when the feed's text looks like a teaser (default)
#   "always" always fetch
#   "never"  only show what the feed sends
# Set it here, per group, or per feed. `f` switches between the two
# versions of an article at any time.
full_article = "auto"

# Show images inside articles. Set to false to skip the terminal graphics
# probe and download no images.
images = true

# The article's first picture sits in the top-left corner with the text
# wrapped beside it, at most this many percent of the pane wide (0 shows it
# above the text instead).
lead_image_width = 40

# Glide over half-page, page and top/bottom jumps instead of snapping.
smooth_scroll = true

# Tallest an image may be, in percent of the article pane's height.
image_max_height = 40

# Disk space for downloaded images, in megabytes. The oldest are removed
# at startup when the cache grows past this.
cache_limit_mb = 200

# Program used to open links. Leave unset for the system default browser.
# browser = "firefox"

[[group]]
name = "Tech"
feeds = [
  { name = "Raspberry Pi", url = "https://www.raspberrypi.com/news/feed/" },
  { name = "Rust Blog", url = "https://blog.rust-lang.org/feed.xml" },
  { name = "Julia Evans", url = "https://jvns.ca/atom.xml" },
  # A teaser-only feed, read in full through full-article fetch:
  { name = "Ars Technica", url = "https://feeds.arstechnica.com/arstechnica/index" },
]

[[group]]
name = "Science"
feeds = [
  { name = "NASA", url = "https://www.nasa.gov/feed/" },
]

[[group]]
name = "Long reads"
feeds = [
  { name = "Bellingcat", url = "https://www.bellingcat.com/feed/" },
  { name = "The Marginalian", url = "https://www.themarginalian.org/feed/" },
]

[[group]]
name = "Comics"
# The comic is the whole post: nothing more to fetch.
full_article = "never"
feeds = [
  { name = "xkcd", url = "https://xkcd.com/atom.xml" },
]
"#;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FullMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeedConfig {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub full_article: Option<FullMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub feeds: Vec<FeedConfig>,
    #[serde(default)]
    pub full_article: Option<FullMode>,
}

fn default_refresh() -> u64 {
    30
}
fn default_true() -> bool {
    true
}
fn default_cache() -> u64 {
    200
}
fn default_lead() -> u16 {
    40
}
fn default_max_h() -> u16 {
    40
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_minutes: u64,
    #[serde(default = "default_true")]
    pub images: bool,
    #[serde(default = "default_cache")]
    pub cache_limit_mb: u64,
    #[serde(default = "default_lead")]
    pub lead_image_width: u16,
    #[serde(default = "default_max_h")]
    pub image_max_height: u16,
    #[serde(default)]
    pub full_article: FullMode,
    #[serde(default = "default_true")]
    pub smooth_scroll: bool,
    #[serde(default)]
    pub browser: Option<String>,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
}

/// Where Feedatui keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub state_file: PathBuf,
    pub image_cache: PathBuf,
    pub article_cache: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        // `FEEDATUI_HOME` keeps everything in one folder (handy for testing).
        if let Some(home) = std::env::var_os("FEEDATUI_HOME") {
            let home = PathBuf::from(home);
            return Ok(Self {
                config_file: home.join("config.toml"),
                state_file: home.join("state.json"),
                image_cache: home.join("cache").join("images"),
                article_cache: home.join("cache").join("articles"),
            });
        }
        let dirs = ProjectDirs::from("", "", "feedatui")
            .context("could not find a home directory for the config")?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            state_file: dirs.data_dir().join("state.json"),
            image_cache: dirs.cache_dir().join("images"),
            article_cache: dirs.cache_dir().join("articles"),
        })
    }
}

impl Config {
    /// Writes the commented default config if there is none yet.
    pub fn ensure(path: &Path) -> Result<()> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, DEFAULT_CONFIG)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }

    /// Loads the config, writing the commented default on first run.
    pub fn load(path: &Path) -> Result<Self> {
        Self::ensure(path)?;
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        cfg.groups.retain(|g| !g.feeds.is_empty());
        cfg.lead_image_width = cfg.lead_image_width.min(70);
        cfg.image_max_height = cfg.image_max_height.clamp(10, 100);
        Ok(cfg)
    }

    /// The full-article setting for a feed: the feed's own, else its
    /// group's, else the global one. A feed in several groups uses the
    /// first group that sets one.
    pub fn full_mode(&self, url: &str) -> FullMode {
        let mut group_mode = None;
        for g in &self.groups {
            for f in &g.feeds {
                if f.url == url {
                    if let Some(m) = f.full_article {
                        return m;
                    }
                    group_mode = group_mode.or(g.full_article);
                }
            }
        }
        group_mode.unwrap_or(self.full_article)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let cfg = Config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.groups.len(), 4);
        assert!(cfg.images);
        assert_eq!(cfg.refresh_minutes, 30);
        assert_eq!(cfg.full_article, FullMode::Auto);
        assert_eq!(cfg.full_mode("https://xkcd.com/atom.xml"), FullMode::Never);
        assert_eq!(cfg.full_mode("https://jvns.ca/atom.xml"), FullMode::Auto);
    }

    #[test]
    fn modes_cascade() {
        let cfg = Config::parse(
            r#"
            full_article = "never"
            [[group]]
            name = "A"
            full_article = "always"
            feeds = [{ url = "a" }, { url = "b", full_article = "auto" }]
            [[group]]
            name = "B"
            feeds = [{ url = "c" }]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.full_mode("a"), FullMode::Always);
        assert_eq!(cfg.full_mode("b"), FullMode::Auto);
        assert_eq!(cfg.full_mode("c"), FullMode::Never);
        assert!(cfg.groups[1].feeds[0].name.is_none());
        assert_eq!(cfg.cache_limit_mb, 200);
    }
}
