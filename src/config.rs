// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! `config.toml`: feed groups and settings, plus where files live.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;

const DEFAULT_CONFIG: &str = r#"# Feedatui configuration
#
# Feeds are organised in groups. Press `b` in the app to pick a group
# (or a single feed inside one); `gt` / `gT` step through the groups.
# `name` is optional: without it the feed's own title is used.

# Minutes between automatic refreshes (0 turns it off).
refresh_minutes = 30

# Show images inside articles. Set to false to skip the terminal graphics
# probe and download no images.
images = true

# Disk space for downloaded images, in megabytes. The oldest are removed
# at startup when the cache grows past this.
cache_limit_mb = 200

# Program used to open links. Leave unset for the system default browser.
# browser = "firefox"

[[group]]
name = "Tech"
feeds = [
  { name = "Ars Technica", url = "https://feeds.arstechnica.com/arstechnica/index" },
  { name = "The Verge", url = "https://www.theverge.com/rss/index.xml" },
  { name = "Hacker News", url = "https://hnrss.org/frontpage" },
]

[[group]]
name = "Science"
feeds = [
  { name = "Quanta", url = "https://www.quantamagazine.org/feed/" },
]

[[group]]
name = "Comics"
feeds = [
  { name = "xkcd", url = "https://xkcd.com/atom.xml" },
]
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct FeedConfig {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub feeds: Vec<FeedConfig>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_minutes: u64,
    #[serde(default = "default_true")]
    pub images: bool,
    #[serde(default = "default_cache")]
    pub cache_limit_mb: u64,
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
            });
        }
        let dirs = ProjectDirs::from("", "", "feedatui")
            .context("could not find a home directory for the config")?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            state_file: dirs.data_dir().join("state.json"),
            image_cache: dirs.cache_dir().join("images"),
        })
    }
}

impl Config {
    /// Loads the config, writing the commented default on first run.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, DEFAULT_CONFIG)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        cfg.groups.retain(|g| !g.feeds.is_empty());
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let cfg = Config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.groups.len(), 3);
        assert_eq!(cfg.groups[0].feeds.len(), 3);
        assert!(cfg.images);
        assert_eq!(cfg.refresh_minutes, 30);
    }

    #[test]
    fn name_is_optional() {
        let cfg = Config::parse(
            r#"
            [[group]]
            name = "A"
            feeds = [{ url = "https://example.com/feed" }]
            "#,
        )
        .unwrap();
        assert!(cfg.groups[0].feeds[0].name.is_none());
        assert_eq!(cfg.cache_limit_mb, 200);
    }
}
