// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Full-article fetch: downloads an article's web page and extracts the
//! article with the Readability algorithm (dom_smoothie), for feeds that only
//! carry a teaser. Results are cached on disk.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use crate::app::Event;

const WORKERS: usize = 3;
/// Extracted articles shorter than this are treated as a failed extraction.
const MIN_TEXT: usize = 200;
/// Below this much text, other places in the page are tried too.
const GOOD_ENOUGH: usize = 1500;
/// Cached articles older than this are fetched again.
const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

#[derive(Clone)]
pub enum FullState {
    Loading,
    Ready(Arc<Fetched>),
    Failed(String),
}

#[derive(Debug)]
pub struct Fetched {
    /// The extracted article HTML.
    pub html: String,
    /// The page URL after redirects, for resolving relative links.
    pub url: String,
}

struct Queue {
    jobs: Mutex<VecDeque<(String, String)>>,
    ready: Condvar,
}

pub struct FullText {
    states: HashMap<String, FullState>,
    queue: Arc<Queue>,
}

impl FullText {
    pub fn new(cache_dir: PathBuf, events: Sender<Event>) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        let client = page_client();
        for _ in 0..WORKERS {
            let queue = Arc::clone(&queue);
            let events = events.clone();
            let client = client.clone();
            let dir = cache_dir.clone();
            std::thread::spawn(move || {
                loop {
                    let (id, url) = {
                        let Ok(mut q) = queue.jobs.lock() else { return };
                        loop {
                            if let Some(job) = q.pop_front() {
                                break job;
                            }
                            q = match queue.ready.wait(q) {
                                Ok(q) => q,
                                Err(_) => return,
                            };
                        }
                    };
                    let result = load(&client, &dir, &id, &url).map_err(|e| format!("{e:#}"));
                    if events.send(Event::FullFetched(id, result)).is_err() {
                        return;
                    }
                }
            });
        }
        Self {
            states: HashMap::new(),
            queue,
        }
    }

    pub fn state(&self, id: &str) -> Option<&FullState> {
        self.states.get(id)
    }

    /// Queues a fetch unless one is done or running. `urgent` jumps the queue
    /// (the article on screen); prefetches go to the back.
    pub fn request(&mut self, id: &str, url: &str, urgent: bool) {
        match self.states.get(id) {
            // Failed stays failed until `retry`.
            Some(FullState::Failed(_)) => return,
            Some(FullState::Ready(_)) | Some(FullState::Loading) => {
                if urgent {
                    // Move a queued prefetch to the front.
                    if let Ok(mut q) = self.queue.jobs.lock()
                        && let Some(pos) = q.iter().position(|j| j.0 == id)
                        && let Some(job) = q.remove(pos)
                    {
                        q.push_front(job);
                    }
                }
                return;
            }
            _ => {}
        }
        self.states.insert(id.to_string(), FullState::Loading);
        if let Ok(mut q) = self.queue.jobs.lock() {
            let job = (id.to_string(), url.to_string());
            if urgent {
                q.push_front(job);
            } else {
                q.push_back(job);
            }
        }
        self.queue.ready.notify_one();
    }

    /// Forgets a failed result so the next request tries again.
    pub fn retry(&mut self, id: &str) {
        if matches!(self.states.get(id), Some(FullState::Failed(_))) {
            self.states.remove(id);
        }
    }

    pub fn on_done(&mut self, id: String, result: Result<Fetched, String>) {
        let state = match result {
            Ok(f) => FullState::Ready(Arc::new(f)),
            Err(e) => FullState::Failed(e),
        };
        self.states.insert(id, state);
    }
}

fn page_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        // A browser-like agent: some sites refuse unknown clients.
        .user_agent(concat!(
            "Mozilla/5.0 (compatible; feedatui/",
            env!("CARGO_PKG_VERSION"),
            ")"
        ))
        .timeout(Duration::from_secs(25))
        .build()
        .expect("HTTP client")
}

fn cache_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.html"))
}

fn load(client: &reqwest::blocking::Client, dir: &Path, id: &str, url: &str) -> Result<Fetched> {
    let path = cache_path(dir, id);
    if let Ok(meta) = std::fs::metadata(&path)
        && meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age < MAX_AGE)
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Some((first, html)) = text.split_once('\n')
    {
        return Ok(Fetched {
            url: first.to_string(),
            html: html.to_string(),
        });
    }

    let resp = client.get(url).send().context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let final_url = resp.url().to_string();
    let is_html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|t| t.contains("html"));
    if !is_html {
        bail!("the link is not a web page");
    }
    let page = resp.text().context("reading the page")?;
    let html = extract(&page, &final_url)?;

    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&path, format!("{final_url}\n{html}"));
    Ok(Fetched {
        html,
        url: final_url,
    })
}

/// Pulls the article out of a web page. If Readability finds little, the
/// page may carry its article elsewhere: in a Next.js data payload (rebuilt
/// into HTML) or as JSON-LD `articleBody`. The longest result wins.
pub fn extract(page: &str, url: &str) -> Result<String> {
    let mut best = readable(page, url);
    if best.as_ref().is_none_or(|(len, _)| *len < GOOD_ENOUGH) {
        let doc = dom_query::Document::from(page);
        let mut candidates = Vec::new();
        if let Some(rebuilt) = crate::nextjs::rebuild(&doc) {
            candidates.extend(readable(&rebuilt, url));
        }
        if let Some(body) = crate::nextjs::json_ld_body(&doc) {
            let len = crate::html::word_count(&body) * 6;
            candidates.push((len, body));
        }
        for c in candidates {
            if best.as_ref().is_none_or(|b| c.0 > b.0) {
                best = Some(c);
            }
        }
    }
    match best {
        Some((len, html)) if len >= MIN_TEXT => Ok(html),
        _ => bail!("no article found on the page"),
    }
}

/// Readability's article and its text length.
fn readable(page: &str, url: &str) -> Option<(usize, String)> {
    let mut r = dom_smoothie::Readability::new(page, Some(url), None).ok()?;
    let article = r.parse().ok()?;
    let len = article.text_content.trim().chars().count();
    Some((len, article.content.to_string()))
}

/// Removes cached articles older than [`MAX_AGE`].
pub fn prune_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > MAX_AGE * 2);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_from_page() {
        let body = "<p>".to_string()
            + &"This is the real article text with plenty of words in it. ".repeat(20)
            + "</p>";
        let page = format!(
            r#"<html><head><title>T</title></head><body>
            <nav><a href="/">Home</a><a href="/a">About</a></nav>
            <article><h1>T</h1>{body}<p><img src="/pic.jpg"></p>{body}</article>
            <footer>Copyright footer links</footer></body></html>"#
        );
        let html = extract(&page, "https://example.com/post").unwrap();
        assert!(html.contains("real article text"));
        assert!(html.contains("pic.jpg"));
        assert!(!html.contains("Copyright footer"));
    }

    #[test]
    fn empty_page_is_an_error() {
        assert!(
            extract(
                "<html><body><p>hi</p></body></html>",
                "https://example.com/"
            )
            .is_err()
        );
    }
}
