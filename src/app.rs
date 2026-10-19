// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! App state and key handling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use image::DynamicImage;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use ratatui_image::sliced::SlicedProtocol;

use crate::article::{self, Doc};
use crate::config::{Config, FullMode, Paths};
use crate::feed::{self, Fetched, Item};
use crate::fulltext::{self, FullState, FullText};
use crate::images::Images;
use crate::store::Store;
use crate::theme;

const FEED_WORKERS: usize = 6;

pub enum Event {
    FeedFetched(String, Result<Fetched, String>),
    ImageDecoded(String, Option<DynamicImage>),
    ImageEncoded(String, Size, bool, Option<SlicedProtocol>),
    FullFetched(String, Result<fulltext::Fetched, String>),
}

/// Which text of an article is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Feed,
    /// Feed text while the full article loads.
    Fetching,
    /// Feed text because the full article could not be had.
    FetchFailed,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Article,
}

/// Which feeds the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Group(usize),
    Feed(usize, usize),
}

/// A row in the group picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerRow {
    All,
    Group(usize),
    Feed(usize, usize),
}

pub enum Popup {
    None,
    Groups {
        cursor: usize,
        expanded: HashSet<usize>,
    },
    Help,
}

pub struct Status {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

/// A queue of feed URLs worked by a few fetch threads.
struct FeedQueue {
    queue: Mutex<VecDeque<String>>,
    ready: Condvar,
}

pub struct App {
    pub cfg: Config,
    pub paths: Paths,
    pub store: Store,
    pub images: Option<Images>,
    pub scope: Scope,
    pub focus: Focus,
    pub popup: Popup,
    /// (feed url, index into that feed's stored items), newest first.
    pub visible: Vec<(String, usize)>,
    pub list: ListState,
    pub scroll: usize,
    /// Rows the current article takes, set while drawing.
    pub article_rows: usize,
    /// The article on screen, keyed by item id and which text it shows.
    doc_cache: Option<(String, Variant, Doc)>,
    pub full: FullText,
    /// `f` toggles: item id → show the full article or not.
    full_view: HashMap<String, bool>,
    /// For telling a held key from single presses.
    motion_at: Option<Instant>,
    fast_until: Option<Instant>,
    pub filter: String,
    pub filtering: bool,
    pub unread_only: bool,
    pub status: Option<Status>,
    pub in_flight: HashSet<String>,
    pub feed_errors: HashMap<String, String>,
    pub last_refresh: Option<Instant>,
    pub list_area: Rect,
    pub article_area: Rect,
    pub quit: bool,
    pending_g: bool,
    /// Until the first key or click, the list stays on the newest article
    /// and nothing is marked read just by being shown.
    interacted: bool,
    feeds: Arc<FeedQueue>,
    dirty: bool,
}

impl App {
    pub fn new(cfg: Config, paths: Paths, images: Option<Images>, events: Sender<Event>) -> Self {
        let store = Store::load(&paths.state_file);
        let feeds = Arc::new(FeedQueue {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        let client = feed::client();
        for _ in 0..FEED_WORKERS {
            let feeds = Arc::clone(&feeds);
            let events = events.clone();
            let client = client.clone();
            std::thread::spawn(move || {
                loop {
                    let url = {
                        let Ok(mut q) = feeds.queue.lock() else {
                            return;
                        };
                        loop {
                            if let Some(u) = q.pop_front() {
                                break u;
                            }
                            q = match feeds.ready.wait(q) {
                                Ok(q) => q,
                                Err(_) => return,
                            };
                        }
                    };
                    let result = feed::fetch(&client, &url).map_err(|e| format!("{e:#}"));
                    if events.send(Event::FeedFetched(url, result)).is_err() {
                        return;
                    }
                }
            });
        }
        let full = FullText::new(paths.article_cache.clone(), events.clone());
        let mut app = Self {
            cfg,
            paths,
            store,
            images,
            scope: Scope::All,
            focus: Focus::List,
            popup: Popup::None,
            visible: Vec::new(),
            list: ListState::default(),
            scroll: 0,
            article_rows: 0,
            doc_cache: None,
            full,
            full_view: HashMap::new(),
            motion_at: None,
            fast_until: None,
            filter: String::new(),
            filtering: false,
            unread_only: false,
            status: None,
            in_flight: HashSet::new(),
            feed_errors: HashMap::new(),
            last_refresh: None,
            list_area: Rect::default(),
            article_area: Rect::default(),
            quit: false,
            pending_g: false,
            interacted: false,
            feeds,
            dirty: false,
        };
        if app.cfg.groups.is_empty() {
            app.set_status(
                format!(
                    "No feeds yet: add some to {}",
                    app.paths.config_file.display()
                ),
                true,
            );
        }
        app.rebuild_list(None);
        app
    }

    // ----- feeds --------------------------------------------------------

    fn scope_urls(&self, scope: Scope) -> Vec<String> {
        let mut urls: Vec<String> = match scope {
            Scope::All => self
                .cfg
                .groups
                .iter()
                .flat_map(|g| g.feeds.iter().map(|f| f.url.clone()))
                .collect(),
            Scope::Group(g) => self.cfg.groups[g]
                .feeds
                .iter()
                .map(|f| f.url.clone())
                .collect(),
            Scope::Feed(g, f) => vec![self.cfg.groups[g].feeds[f].url.clone()],
        };
        let mut seen = HashSet::new();
        urls.retain(|u| seen.insert(u.clone()));
        urls
    }

    pub fn refresh(&mut self, scope: Scope) {
        let urls = self.scope_urls(scope);
        let Ok(mut q) = self.feeds.queue.lock() else {
            return;
        };
        for url in urls {
            if self.in_flight.insert(url.clone()) {
                q.push_back(url);
            }
        }
        drop(q);
        self.feeds.ready.notify_all();
        self.last_refresh = Some(Instant::now());
    }

    pub fn auto_refresh(&mut self) {
        let every = self.cfg.refresh_minutes;
        if every == 0 || !self.in_flight.is_empty() {
            return;
        }
        if self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(every * 60))
        {
            self.refresh(Scope::All);
        }
    }

    pub fn feed_name(&self, url: &str) -> String {
        self.cfg
            .groups
            .iter()
            .flat_map(|g| &g.feeds)
            .find(|f| f.url == url)
            .and_then(|f| f.name.clone())
            .or_else(|| self.store.feed_titles.get(url).cloned())
            .unwrap_or_else(|| {
                reqwest::Url::parse(url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_else(|| url.to_string())
            })
    }

    pub fn scope_name(&self) -> String {
        match self.scope {
            Scope::All => "All feeds".to_string(),
            Scope::Group(g) => self.cfg.groups[g].name.clone(),
            Scope::Feed(g, f) => format!(
                "{} › {}",
                self.cfg.groups[g].name,
                self.feed_name(&self.cfg.groups[g].feeds[f].url)
            ),
        }
    }

    pub fn unread_in(&self, urls: &[String]) -> usize {
        urls.iter()
            .filter_map(|u| self.store.items.get(u))
            .flatten()
            .filter(|i| !self.store.read.contains(&i.id))
            .count()
    }

    pub fn unread_for(&self, row: PickerRow) -> usize {
        let scope = match row {
            PickerRow::All => Scope::All,
            PickerRow::Group(g) => Scope::Group(g),
            PickerRow::Feed(g, f) => Scope::Feed(g, f),
        };
        self.unread_in(&self.scope_urls(scope))
    }

    // ----- list ---------------------------------------------------------

    pub fn item(&self, (url, i): &(String, usize)) -> Option<&Item> {
        self.store.items.get(url).and_then(|v| v.get(*i))
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.list
            .selected()
            .and_then(|s| self.visible.get(s))
            .and_then(|r| self.item(r))
    }

    /// Recomputes the visible list, keeping the selection on `keep` (an item id)
    /// or on the currently selected item.
    pub fn rebuild_list(&mut self, keep: Option<String>) {
        let keep = if self.interacted {
            keep.or_else(|| self.selected_item().map(|i| i.id.clone()))
        } else {
            None
        };
        let urls = self.scope_urls(self.scope);
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<(String, usize)> = Vec::new();
        for url in &urls {
            let Some(items) = self.store.items.get(url) else {
                continue;
            };
            let source = self.feed_name(url).to_lowercase();
            for (i, it) in items.iter().enumerate() {
                let is_kept = keep.as_deref() == Some(it.id.as_str());
                if self.unread_only && self.store.read.contains(&it.id) && !is_kept {
                    continue;
                }
                if !needle.is_empty()
                    && !it.title.to_lowercase().contains(&needle)
                    && !source.contains(&needle)
                {
                    continue;
                }
                rows.push((url.clone(), i));
            }
        }
        rows.sort_by(|a, b| {
            let pa = self.item(a).and_then(|i| i.published);
            let pb = self.item(b).and_then(|i| i.published);
            pb.cmp(&pa)
        });
        self.visible = rows;
        let idx = keep
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|r| self.item(r).is_some_and(|i| i.id == id))
            })
            .or(if self.visible.is_empty() {
                None
            } else {
                Some(0)
            });
        let before = self.selected_item().map(|i| i.id.clone());
        self.list.select(idx);
        // A rebuild (refresh, group switch, filter) never marks anything read;
        // only moving onto an article or opening it does.
        if self.selected_item().map(|i| i.id.clone()) != before {
            self.scroll = 0;
        }
    }

    fn select(&mut self, idx: usize) {
        if self.visible.is_empty() {
            return;
        }
        let idx = idx.min(self.visible.len() - 1);
        if Some(idx) != self.list.selected() {
            self.list.select(Some(idx));
            self.on_selection_changed();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.select((cur + delta).max(0) as usize);
    }

    fn on_selection_changed(&mut self) {
        self.motion();
        self.scroll = 0;
        self.mark_selected_read();
        self.prefetch();
    }

    fn mark_selected_read(&mut self) {
        if !self.interacted {
            return;
        }
        if let Some(id) = self.selected_item().map(|i| i.id.clone())
            && self.store.read.insert(id)
        {
            self.dirty = true;
        }
    }

    fn toggle_read(&mut self) {
        if let Some(id) = self.selected_item().map(|i| i.id.clone()) {
            if !self.store.read.remove(&id) {
                self.store.read.insert(id);
            }
            self.dirty = true;
        }
    }

    // ----- article ------------------------------------------------------

    /// Whether an article should show its full, fetched version.
    pub fn wants_full(&self, item: &Item) -> bool {
        if item.link.is_none() {
            return false;
        }
        if let Some(v) = self.full_view.get(&item.id) {
            return *v;
        }
        match self.cfg.full_mode(&item.feed_url) {
            FullMode::Always => true,
            FullMode::Never => false,
            FullMode::Auto => crate::html::looks_truncated(&item.body),
        }
    }

    fn variant(&self, item: &Item) -> Variant {
        if !self.wants_full(item) {
            return Variant::Feed;
        }
        match self.full.state(&item.id) {
            Some(FullState::Ready(_)) => Variant::Full,
            Some(FullState::Failed(_)) => Variant::FetchFailed,
            _ => Variant::Fetching,
        }
    }

    fn build_doc(&self, item: &Item, variant: Variant) -> Doc {
        let source = self.feed_name(&item.feed_url);
        let full = match self.full.state(&item.id) {
            Some(FullState::Ready(f)) if variant == Variant::Full => Some(Arc::clone(f)),
            _ => None,
        };
        let mut doc = match &full {
            Some(f) => article::build(item, &source, &f.html, Some(&f.url)),
            None => article::build(item, &source, &item.body, None),
        };
        let meta = theme::meta();
        let accent = Style::default().fg(theme::ACCENT);
        doc.note = match variant {
            Variant::Full => {
                let host = full
                    .as_ref()
                    .and_then(|f| reqwest::Url::parse(&f.url).ok())
                    .and_then(|u| {
                        u.host_str()
                            .map(|h| h.trim_start_matches("www.").to_string())
                    })
                    .unwrap_or_default();
                Some(Line::from(vec![
                    Span::styled("◆ ", accent),
                    Span::styled(format!("Full article from {host} · "), meta),
                    Span::styled("f", theme::key()),
                    Span::styled(" shows the feed's text", meta),
                ]))
            }
            Variant::Fetching => Some(Line::from(vec![
                Span::styled("◌ ", accent),
                Span::styled("Fetching the full article…", meta),
            ])),
            Variant::FetchFailed => {
                let err = match self.full.state(&item.id) {
                    Some(FullState::Failed(e)) => e.clone(),
                    _ => String::new(),
                };
                // Loud only when asked for with `f`; quiet when automatic.
                let asked = self.full_view.get(&item.id) == Some(&true);
                let style = if asked { theme::error() } else { meta };
                Some(Line::from(vec![
                    Span::styled("✗ ", theme::error()),
                    Span::styled(
                        format!("Feed text: the full article couldn't be fetched ({err}) · "),
                        style,
                    ),
                    Span::styled("f", theme::key()),
                    Span::styled(" tries again", meta),
                ]))
            }
            Variant::Feed if item.link.is_some() => Some(Line::from(vec![
                Span::styled("Feed text · ", meta),
                Span::styled("f", theme::key()),
                Span::styled(" fetches the full article", meta),
            ])),
            Variant::Feed => None,
        };
        doc
    }

    /// The article on screen, rebuilt when its text changes. Starts the
    /// full-article fetch and image downloads it needs.
    pub fn current_doc(&mut self) -> Option<&Doc> {
        let item = self.selected_item()?.clone();
        if self.wants_full(&item)
            && let Some(link) = &item.link
        {
            self.full.request(&item.id, link, true);
        }
        let variant = self.variant(&item);
        let fresh = self
            .doc_cache
            .as_ref()
            .is_some_and(|(id, v, _)| *id == item.id && *v == variant);
        if !fresh {
            let doc = self.build_doc(&item, variant);
            if let Some(images) = &mut self.images {
                images.show(&doc.images);
            }
            self.doc_cache = Some((item.id.clone(), variant, doc));
        }
        self.doc_cache.as_ref().map(|(_, _, d)| d)
    }

    /// Gets the next articles ready: their full text and their images.
    fn prefetch(&mut self) {
        let Some(sel) = self.list.selected() else {
            return;
        };
        let next: Vec<Item> = (sel + 1..sel + 3)
            .filter_map(|i| self.visible.get(i))
            .filter_map(|r| self.item(r))
            .cloned()
            .collect();
        for item in next {
            if self.wants_full(&item) {
                if let Some(link) = &item.link {
                    self.full.request(&item.id, link, false);
                }
                if matches!(self.full.state(&item.id), Some(FullState::Ready(_))) {
                    self.prefetch_images(&item, Variant::Full);
                }
            } else {
                self.prefetch_images(&item, Variant::Feed);
            }
        }
    }

    fn prefetch_images(&mut self, item: &Item, variant: Variant) {
        if self.images.is_none() {
            return;
        }
        let doc = self.build_doc(item, variant);
        if let Some(images) = &mut self.images {
            images.prefetch(&doc.images);
        }
    }

    fn toggle_full(&mut self) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        if item.link.is_none() {
            self.set_status("This article has no link to fetch".into(), true);
            return;
        }
        let failed = matches!(self.full.state(&item.id), Some(FullState::Failed(_)));
        let show = if failed && self.wants_full(&item) {
            // Retry rather than switch off.
            true
        } else {
            !self.wants_full(&item)
        };
        if show {
            self.full.retry(&item.id);
        }
        self.full_view.insert(item.id.clone(), show);
        self.scroll = 0;
        self.mark_selected_read();
    }

    /// Notes movement; a burst of it (a held key) counts as fast scrolling.
    fn motion(&mut self) {
        let now = Instant::now();
        if self
            .motion_at
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(150))
        {
            self.fast_until = Some(now + Duration::from_millis(220));
        }
        self.motion_at = Some(now);
    }

    /// Whether the reader is scrolling fast enough that heavy images should
    /// wait for a pause.
    pub fn fast_scrolling(&self) -> bool {
        self.fast_until.is_some_and(|t| Instant::now() < t)
    }

    /// True once when fast scrolling has just ended (time to draw again).
    pub fn settle_due(&mut self) -> bool {
        if self.fast_until.is_some_and(|t| Instant::now() >= t) {
            self.fast_until = None;
            return true;
        }
        false
    }

    fn article_page(&self) -> usize {
        usize::from(self.article_area.height.saturating_sub(2)).max(1)
    }

    fn scroll_by(&mut self, delta: isize) {
        self.motion();
        let max = self.article_rows.saturating_sub(self.article_page());
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    fn open_link(&mut self) {
        let Some(link) = self.selected_item().and_then(|i| i.link.clone()) else {
            self.set_status("This article has no link".into(), true);
            return;
        };
        let result = match &self.cfg.browser {
            Some(b) => open::with_detached(&link, b),
            None => open::that_detached(&link),
        };
        match result {
            Ok(()) => {
                self.mark_selected_read();
                self.set_status(format!("Opened {link}"), false)
            }
            Err(e) => self.set_status(format!("Could not open the browser: {e}"), true),
        }
    }

    // ----- events -------------------------------------------------------

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::FeedFetched(url, result) => {
                self.in_flight.remove(&url);
                match result {
                    Ok(fetched) => {
                        self.feed_errors.remove(&url);
                        if let Some(t) = fetched.title.filter(|t| !t.is_empty()) {
                            self.store.feed_titles.insert(url.clone(), t);
                        }
                        self.store.merge(&url, fetched.items);
                        self.doc_cache = None;
                        self.rebuild_list(None);
                        self.prefetch();
                        self.dirty = true;
                    }
                    Err(e) => {
                        let name = self.feed_name(&url);
                        self.set_status(format!("{name}: {e}"), true);
                        self.feed_errors.insert(url, e);
                    }
                }
                if self.in_flight.is_empty() {
                    self.save();
                }
            }
            Event::ImageDecoded(url, img) => {
                if let Some(images) = &mut self.images {
                    images.on_decoded(url, img);
                }
            }
            Event::ImageEncoded(url, size, standin, proto) => {
                if let Some(images) = &mut self.images {
                    images.on_encoded(url, size, standin, proto);
                }
            }
            Event::FullFetched(id, result) => {
                let ok = result.is_ok();
                self.full.on_done(id.clone(), result);
                // A prefetched article: fetch its images too.
                let current = self.selected_item().is_some_and(|i| i.id == id);
                if ok
                    && !current
                    && let Some(item) = self
                        .visible
                        .iter()
                        .filter_map(|r| self.item(r))
                        .find(|i| i.id == id)
                        .cloned()
                {
                    self.prefetch_images(&item, Variant::Full);
                }
            }
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        match self.store.save(&self.paths.state_file) {
            Ok(()) => self.dirty = false,
            Err(e) => self.set_status(format!("Saving state failed: {e:#}"), true),
        }
    }

    pub fn set_status(&mut self, text: String, error: bool) {
        self.status = Some(Status {
            text,
            error,
            at: Instant::now(),
        });
    }

    // ----- group picker -------------------------------------------------

    pub fn picker_rows(&self, expanded: &HashSet<usize>) -> Vec<PickerRow> {
        let mut rows = vec![PickerRow::All];
        for (gi, g) in self.cfg.groups.iter().enumerate() {
            rows.push(PickerRow::Group(gi));
            if expanded.contains(&gi) {
                rows.extend((0..g.feeds.len()).map(|fi| PickerRow::Feed(gi, fi)));
            }
        }
        rows
    }

    fn open_picker(&mut self) {
        let mut expanded = HashSet::new();
        if let Scope::Feed(g, _) = self.scope {
            expanded.insert(g);
        }
        let current = match self.scope {
            Scope::All => PickerRow::All,
            Scope::Group(g) => PickerRow::Group(g),
            Scope::Feed(g, f) => PickerRow::Feed(g, f),
        };
        let cursor = self
            .picker_rows(&expanded)
            .iter()
            .position(|r| *r == current)
            .unwrap_or(0);
        self.popup = Popup::Groups { cursor, expanded };
    }

    fn set_scope(&mut self, scope: Scope) {
        if self.scope != scope {
            self.scope = scope;
            self.filter.clear();
            self.list.select(None);
            self.rebuild_list(None);
        }
    }

    fn cycle_group(&mut self, delta: isize) {
        // gt / gT walk All → groups → All, like tabs.
        let n = self.cfg.groups.len() as isize + 1;
        let cur = match self.scope {
            Scope::All => 0,
            Scope::Group(g) | Scope::Feed(g, _) => g as isize + 1,
        };
        let next = (cur + delta).rem_euclid(n);
        self.set_scope(if next == 0 {
            Scope::All
        } else {
            Scope::Group(next as usize - 1)
        });
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Popup::Groups {
            mut cursor,
            mut expanded,
        } = std::mem::replace(&mut self.popup, Popup::None)
        else {
            return;
        };
        let rows = self.picker_rows(&expanded);
        let mut close = false;
        let mut chosen: Option<PickerRow> = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') => close = true,
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(rows.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => cursor = 0,
            KeyCode::Char('G') | KeyCode::End => cursor = rows.len() - 1,
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => {
                if let PickerRow::Group(g) = rows[cursor]
                    && !expanded.insert(g)
                    && key.code == KeyCode::Tab
                {
                    expanded.remove(&g);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => match rows[cursor] {
                PickerRow::Group(g) => {
                    expanded.remove(&g);
                }
                PickerRow::Feed(g, _) => {
                    expanded.remove(&g);
                    cursor = self
                        .picker_rows(&expanded)
                        .iter()
                        .position(|r| *r == PickerRow::Group(g))
                        .unwrap_or(0);
                }
                PickerRow::All => {}
            },
            KeyCode::Enter | KeyCode::Char(' ') => chosen = Some(rows[cursor]),
            KeyCode::Char(c @ '0'..='9') => {
                let n = c as usize - '0' as usize;
                if n == 0 {
                    chosen = Some(PickerRow::All);
                } else if n <= self.cfg.groups.len() {
                    chosen = Some(PickerRow::Group(n - 1));
                }
            }
            _ => {}
        }
        if let Some(row) = chosen {
            self.set_scope(match row {
                PickerRow::All => Scope::All,
                PickerRow::Group(g) => Scope::Group(g),
                PickerRow::Feed(g, f) => Scope::Feed(g, f),
            });
            self.focus = Focus::List;
            close = true;
        }
        if !close {
            self.popup = Popup::Groups { cursor, expanded };
        }
    }

    // ----- keys ---------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        self.interact();
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        match self.popup {
            Popup::Groups { .. } => return self.picker_key(key),
            Popup::Help => {
                self.popup = Popup::None;
                return;
            }
            Popup::None => {}
        }
        if self.filtering {
            return self.filter_key(key);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if std::mem::take(&mut self.pending_g) {
            match key.code {
                KeyCode::Char('g') => self.go_top(),
                KeyCode::Char('t') => self.cycle_group(1),
                KeyCode::Char('T') => self.cycle_group(-1),
                _ => {}
            }
            return;
        }

        let half = (self.article_page() / 2).max(1) as isize;
        let list_half = (usize::from(self.list_area.height.saturating_sub(2)) / 4).max(1) as isize;
        match (key.code, ctrl) {
            (KeyCode::Char('q'), false) => self.quit = true,
            (KeyCode::Char('?'), false) => self.popup = Popup::Help,
            (KeyCode::Char('b'), false) => self.open_picker(),
            (KeyCode::Char('g'), false) => self.pending_g = true,
            (KeyCode::Char('G'), false) | (KeyCode::End, _) => self.go_bottom(),
            (KeyCode::Home, _) => self.go_top(),

            (KeyCode::Char('h'), false) | (KeyCode::Left, _) => self.focus = Focus::List,
            (KeyCode::Char('l'), false) | (KeyCode::Right, _) | (KeyCode::Enter, _) => {
                self.focus_article()
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => match self.focus {
                Focus::List => self.focus_article(),
                Focus::Article => self.focus = Focus::List,
            },

            (KeyCode::Char('j'), false) | (KeyCode::Down, _) => self.down(1),
            (KeyCode::Char('k'), false) | (KeyCode::Up, _) => self.down(-1),
            (KeyCode::Char('d'), true) => self.page(half, list_half),
            (KeyCode::Char('u'), true) => self.page(-half, -list_half),
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) | (KeyCode::Char(' '), false) => {
                self.page(half * 2 - 1, list_half * 2)
            }
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => {
                self.page(-(half * 2 - 1), -list_half * 2)
            }
            (KeyCode::Char('J'), false) => self.move_selection(1),
            (KeyCode::Char('K'), false) => self.move_selection(-1),

            (KeyCode::Char('o'), false) => self.open_link(),
            (KeyCode::Char('r'), false) => {
                self.refresh(self.scope);
                self.set_status(format!("Refreshing {}", self.scope_name()), false);
            }
            (KeyCode::Char('R'), false) => {
                self.refresh(Scope::All);
                self.set_status("Refreshing all feeds".into(), false);
            }
            (KeyCode::Char('m'), false) => self.toggle_read(),
            (KeyCode::Char('f'), false) => self.toggle_full(),
            (KeyCode::Char('x'), false) => {
                self.unread_only = !self.unread_only;
                self.rebuild_list(None);
            }
            (KeyCode::Char('/'), false) => {
                self.filtering = true;
                self.filter.clear();
                self.focus = Focus::List;
                self.rebuild_list(None);
            }
            (KeyCode::Esc, _) if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild_list(None);
            }
            _ => {}
        }
    }

    fn filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
            }
            KeyCode::Enter => self.filtering = false,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(c)
            }
            _ => return,
        }
        self.rebuild_list(None);
    }

    fn down(&mut self, n: isize) {
        match self.focus {
            Focus::List => self.move_selection(n),
            Focus::Article => self.scroll_by(n),
        }
    }

    fn page(&mut self, article: isize, list: isize) {
        match self.focus {
            Focus::List => self.move_selection(list),
            Focus::Article => self.scroll_by(article),
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Focus::List => self.select(0),
            Focus::Article => self.scroll = 0,
        }
    }

    fn go_bottom(&mut self) {
        match self.focus {
            Focus::List => self.select(self.visible.len().saturating_sub(1)),
            Focus::Article => self.scroll_by(isize::MAX / 2),
        }
    }

    fn interact(&mut self) {
        self.interacted = true;
    }

    fn focus_article(&mut self) {
        if self.selected_item().is_some() {
            self.focus = Focus::Article;
            self.mark_selected_read();
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if matches!(m.kind, MouseEventKind::Down(_)) {
            self.interact();
        }
        if !matches!(self.popup, Popup::None) {
            return;
        }
        let pos = Position::new(m.column, m.row);
        let in_list = self.list_area.contains(pos);
        let in_article = self.article_area.contains(pos);
        match m.kind {
            MouseEventKind::ScrollDown if in_list => self.move_selection(1),
            MouseEventKind::ScrollUp if in_list => self.move_selection(-1),
            MouseEventKind::ScrollDown if in_article => self.scroll_by(3),
            MouseEventKind::ScrollUp if in_article => self.scroll_by(-3),
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                self.focus = Focus::List;
                let row = m.row.saturating_sub(self.list_area.y + 1) as usize;
                let idx = self.list.offset() + row / 2;
                if row < usize::from(self.list_area.height.saturating_sub(2)) {
                    self.select(idx);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_article => self.focus_article(),
            _ => {}
        }
    }
}
