// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Article images: download (with a disk cache), decode and encode for the
//! terminal's graphics protocol off the UI thread, drawn with ratatui-image.
//! Sixel, Kitty and iTerm2 terminals get real pixels; others get half-blocks.
//!
//! The article on screen goes first; images of the next articles are fetched
//! in the background. Sixel and iTerm2 images are large to send, so while
//! the reader scrolls quickly a half-block copy stands in and the real image
//! is sent once scrolling pauses.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::layout::Size;
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;

use crate::app::Event;
use crate::feed::fnv1a;

/// Decoded images are kept at most this wide (enough for any pane).
const MAX_DECODED_WIDTH: u32 = 1400;
/// Images smaller than this (tracking pixels, spacers) are left out.
const MIN_SIDE: u32 = 8;
const WORKERS: usize = 6;
/// Decoded images kept in memory.
const KEEP_DECODED: usize = 48;

pub enum ImgState {
    Loading,
    Ready(Arc<DynamicImage>),
    Failed,
}

enum Job {
    Fetch(String),
    Encode {
        url: String,
        image: Arc<DynamicImage>,
        size: Size,
        standin: bool,
    },
}

struct Queue {
    urgent: VecDeque<Job>,
    background: VecDeque<Job>,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

/// An encoded image for one size; `None` while the encode is running.
struct Encoded {
    size: Size,
    proto: Option<SlicedProtocol>,
}

pub struct Images {
    picker: Picker,
    states: HashMap<String, ImgState>,
    /// Use order of decoded images, oldest first.
    lru: VecDeque<String>,
    encoded: HashMap<(String, bool), Encoded>,
    pinned: HashSet<String>,
    shared: Arc<Shared>,
    /// Bumped whenever an image finishes loading, so layouts know to redo.
    pub version: u64,
}

impl Images {
    pub fn new(picker: Picker, cache_dir: PathBuf, events: Sender<Event>) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                urgent: VecDeque::new(),
                background: VecDeque::new(),
            }),
            ready: Condvar::new(),
        });
        let client = crate::feed::client();
        // Same cell size as the real picker so stand-ins line up exactly.
        let standin = halfblocks_like(&picker);
        for _ in 0..WORKERS {
            let shared = Arc::clone(&shared);
            let events = events.clone();
            let client = client.clone();
            let picker = picker.clone();
            let standin = standin.clone();
            let cache_dir = cache_dir.clone();
            std::thread::spawn(move || {
                worker(&shared, &events, &client, &picker, &standin, &cache_dir)
            });
        }
        Self {
            picker,
            states: HashMap::new(),
            lru: VecDeque::new(),
            encoded: HashMap::new(),
            pinned: HashSet::new(),
            shared,
            version: 0,
        }
    }

    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    /// Whether this protocol is costly to resend while scrolling.
    pub fn heavy(&self) -> bool {
        matches!(
            self.picker.protocol_type(),
            ProtocolType::Sixel | ProtocolType::Iterm2
        )
    }

    pub fn state(&self, url: &str) -> Option<&ImgState> {
        self.states.get(url)
    }

    fn push(&self, job: Job, urgent: bool) {
        if let Ok(mut q) = self.shared.queue.lock() {
            if urgent {
                q.urgent.push_back(job);
            } else {
                q.background.push_back(job);
            }
        }
        self.shared.ready.notify_one();
    }

    /// The article on screen: fetch its images first and keep them in memory.
    pub fn show(&mut self, urls: &[String]) {
        self.pinned = urls.iter().cloned().collect();
        self.encoded.retain(|(url, _), _| urls.contains(url));
        // Promote anything of this article still waiting in the background.
        if let Ok(mut q) = self.shared.queue.lock() {
            let mut keep = VecDeque::new();
            while let Some(job) = q.background.pop_front() {
                match &job {
                    Job::Fetch(u) if urls.contains(u) => q.urgent.push_back(job),
                    _ => keep.push_back(job),
                }
            }
            q.background = keep;
        }
        for url in urls {
            if !self.states.contains_key(url) {
                self.states.insert(url.clone(), ImgState::Loading);
                self.push(Job::Fetch(url.clone()), true);
            }
        }
        self.trim();
    }

    /// Images of articles the reader is likely to open next.
    pub fn prefetch(&mut self, urls: &[String]) {
        for url in urls {
            if !self.states.contains_key(url) {
                self.states.insert(url.clone(), ImgState::Loading);
                self.push(Job::Fetch(url.clone()), false);
            }
        }
    }

    fn trim(&mut self) {
        while self.lru.len() > KEEP_DECODED {
            let Some(pos) = self.lru.iter().position(|u| !self.pinned.contains(u)) else {
                break;
            };
            if let Some(url) = self.lru.remove(pos) {
                self.states.remove(&url);
            }
        }
    }

    pub fn on_decoded(&mut self, url: String, result: Option<DynamicImage>) {
        let state = match result {
            Some(img) if img.width() >= MIN_SIDE && img.height() >= MIN_SIDE => {
                self.lru.retain(|u| u != &url);
                self.lru.push_back(url.clone());
                ImgState::Ready(Arc::new(img))
            }
            _ => ImgState::Failed,
        };
        self.states.insert(url, state);
        self.version += 1;
        self.trim();
    }

    pub fn on_encoded(
        &mut self,
        url: String,
        size: Size,
        standin: bool,
        proto: Option<SlicedProtocol>,
    ) {
        let key = (url, standin);
        let Some(e) = self.encoded.get_mut(&key) else {
            return; // freed meanwhile
        };
        if e.size != size {
            return; // a newer size was requested meanwhile
        }
        match proto {
            Some(p) => e.proto = Some(p),
            None => {
                self.encoded.remove(&key);
                self.states.insert(key.0, ImgState::Failed);
                self.version += 1;
            }
        }
    }

    /// The encoded image at `size`, starting an encode if needed. With
    /// `standin`, a quick half-block version (for fast scrolling).
    pub fn protocol(&mut self, url: &str, size: Size, standin: bool) -> Option<&SlicedProtocol> {
        let Some(ImgState::Ready(image)) = self.states.get(url) else {
            return None;
        };
        let key = (url.to_string(), standin);
        let stale = self.encoded.get(&key).is_none_or(|e| e.size != size);
        if stale {
            let image = Arc::clone(image);
            self.encoded
                .insert(key.clone(), Encoded { size, proto: None });
            self.push(
                Job::Encode {
                    url: url.to_string(),
                    image,
                    size,
                    standin,
                },
                true,
            );
            return None;
        }
        self.encoded.get(&key).and_then(|e| e.proto.as_ref())
    }
}

fn halfblocks_like(picker: &Picker) -> Picker {
    let mut p = picker.clone();
    p.set_protocol_type(ProtocolType::Halfblocks);
    p
}

/// Cell size an image gets in a box at most `max_w` × `max_h` cells: its
/// natural size, shrunk to fit, aspect kept.
pub fn fit_size(img: &DynamicImage, font: FontSize, max_w: u16, max_h: u16) -> Size {
    fit_dims(img.width(), img.height(), font, max_w, max_h)
}

pub fn fit_dims(iw: u32, ih: u32, font: FontSize, max_w: u16, max_h: u16) -> Size {
    let (iw, ih) = (iw.max(1) as f64, ih.max(1) as f64);
    let (fw, fh) = (font.width.max(1) as f64, font.height.max(1) as f64);
    let natural_w = (iw / fw).ceil();
    let mut w = natural_w.min(max_w as f64).max(1.0);
    let mut h = (w * fw * ih / iw / fh).round().max(1.0);
    if h > max_h as f64 {
        h = (max_h as f64).max(1.0);
        w = (h * fh * iw / ih / fw).round().clamp(1.0, max_w as f64);
    }
    Size::new(w as u16, h as u16)
}

fn worker(
    shared: &Shared,
    events: &Sender<Event>,
    client: &reqwest::blocking::Client,
    picker: &Picker,
    standin_picker: &Picker,
    cache_dir: &Path,
) {
    loop {
        let job = {
            let Ok(mut q) = shared.queue.lock() else {
                return;
            };
            loop {
                if let Some(j) = q.urgent.pop_front().or_else(|| q.background.pop_front()) {
                    break j;
                }
                q = match shared.ready.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let event = match job {
            Job::Fetch(url) => {
                let img = load(client, cache_dir, &url);
                Event::ImageDecoded(url, img)
            }
            Job::Encode {
                url,
                image,
                size,
                standin,
            } => {
                let p = if standin { standin_picker } else { picker };
                let font = p.font_size();
                let px_w = u32::from(size.width) * u32::from(font.width);
                let px_h = u32::from(size.height) * u32::from(font.height);
                let scaled = image.resize(px_w, px_h, FilterType::CatmullRom);
                let proto = SlicedProtocol::new(p, scaled, Some(size)).ok();
                Event::ImageEncoded(url, size, standin, proto)
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

fn cache_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(format!("{:016x}.img", fnv1a(url.as_bytes())))
}

fn load(client: &reqwest::blocking::Client, dir: &Path, url: &str) -> Option<DynamicImage> {
    let path = cache_path(dir, url);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            let resp = client.get(url).send().ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let b = resp.bytes().ok()?.to_vec();
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&path, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    Some(if img.width() > MAX_DECODED_WIDTH {
        // `thumbnail` is a fast box filter, fine for a large downscale.
        let h = (u64::from(img.height()) * u64::from(MAX_DECODED_WIDTH) / u64::from(img.width()))
            .max(1) as u32;
        img.thumbnail(MAX_DECODED_WIDTH, h)
    } else {
        img
    })
}

/// Deletes the least recently written cached images until the cache fits.
pub fn prune_cache(dir: &Path, limit_mb: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| {
                (
                    m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    m.len(),
                    e.path(),
                )
            })
        })
        .collect();
    let limit = limit_mb.saturating_mul(1024 * 1024);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_bounds() {
        let font = FontSize::new(10, 20);
        let img = DynamicImage::new_rgb8(800, 400);
        // Natural: 80 cols; at 40 cols the height is 40*10*400/800/20 = 10 rows.
        assert_eq!(fit_size(&img, font, 40, 100), Size::new(40, 10));
        // Small images keep their natural size.
        let small = DynamicImage::new_rgb8(100, 100);
        assert_eq!(fit_size(&small, font, 80, 100), Size::new(10, 5));
        // Tall images are capped by height.
        let tall = DynamicImage::new_rgb8(200, 2000);
        let s = fit_size(&tall, font, 80, 20);
        assert_eq!(s.height, 20);
        assert_eq!(s.width, 4);
    }
}
