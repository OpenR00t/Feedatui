// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Article images: download (with a disk cache), decode and encode for the
//! terminal's graphics protocol off the UI thread, drawn with ratatui-image.
//! Sixel, Kitty and iTerm2 terminals get real pixels; others get half-blocks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::layout::Size;
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

use crate::app::Event;
use crate::feed::fnv1a;

/// Decoded images wider than this are scaled down right away to save memory.
const MAX_DECODED_WIDTH: u32 = 1600;
/// Images smaller than this (tracking pixels, spacers) are left out.
const MIN_SIDE: u32 = 8;
const WORKERS: usize = 4;

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
    },
}

/// An encoded image for one size; `None` while the encode is running.
struct Encoded {
    size: Size,
    proto: Option<SlicedProtocol>,
}

pub struct Images {
    picker: Picker,
    states: HashMap<String, ImgState>,
    encoded: HashMap<String, Encoded>,
    jobs: Sender<Job>,
}

impl Images {
    pub fn new(picker: Picker, cache_dir: PathBuf, events: Sender<Event>) -> Self {
        let (jobs, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let client = crate::feed::client();
        for _ in 0..WORKERS {
            let rx = Arc::clone(&rx);
            let events = events.clone();
            let client = client.clone();
            let picker = picker.clone();
            let cache_dir = cache_dir.clone();
            std::thread::spawn(move || worker(&rx, &events, &client, &picker, &cache_dir));
        }
        Self {
            picker,
            states: HashMap::new(),
            encoded: HashMap::new(),
            jobs,
        }
    }

    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    pub fn state(&self, url: &str) -> Option<&ImgState> {
        self.states.get(url)
    }

    /// Starts downloading any of `urls` not seen yet.
    pub fn request(&mut self, urls: &[String]) {
        for url in urls {
            if !self.states.contains_key(url) {
                self.states.insert(url.clone(), ImgState::Loading);
                let _ = self.jobs.send(Job::Fetch(url.clone()));
            }
        }
    }

    /// Frees images that are not part of the article being read.
    pub fn retain(&mut self, keep: &[String]) {
        let keep: HashSet<&str> = keep.iter().map(String::as_str).collect();
        self.states
            .retain(|url, s| !matches!(s, ImgState::Ready(_)) || keep.contains(url.as_str()));
        self.encoded.retain(|url, _| keep.contains(url.as_str()));
    }

    pub fn on_decoded(&mut self, url: String, result: Option<DynamicImage>) {
        let state = match result {
            Some(img) if img.width() >= MIN_SIDE && img.height() >= MIN_SIDE => {
                ImgState::Ready(Arc::new(img))
            }
            _ => ImgState::Failed,
        };
        self.states.insert(url, state);
    }

    pub fn on_encoded(&mut self, url: String, size: Size, proto: Option<SlicedProtocol>) {
        let Some(e) = self.encoded.get_mut(&url) else {
            return; // freed meanwhile
        };
        if e.size != size {
            return; // a newer size was requested meanwhile
        }
        match proto {
            Some(p) => e.proto = Some(p),
            None => {
                // Encoding failed: treat the image as unavailable.
                self.encoded.remove(&url);
                self.states.insert(url, ImgState::Failed);
            }
        }
    }

    /// The encoded image at `size`, starting an encode if needed.
    pub fn protocol(&mut self, url: &str, size: Size) -> Option<&SlicedProtocol> {
        let Some(ImgState::Ready(image)) = self.states.get(url) else {
            return None;
        };
        let stale = self.encoded.get(url).is_none_or(|e| e.size != size);
        if stale {
            self.encoded
                .insert(url.to_string(), Encoded { size, proto: None });
            let _ = self.jobs.send(Job::Encode {
                url: url.to_string(),
                image: Arc::clone(image),
                size,
            });
            return None;
        }
        self.encoded.get(url).and_then(|e| e.proto.as_ref())
    }
}

/// Cell size an image gets in a column `max_w` cells wide and at most
/// `max_h` rows tall: its natural size, shrunk to fit, aspect kept.
pub fn fit_size(img: &DynamicImage, font: FontSize, max_w: u16, max_h: u16) -> Size {
    let (iw, ih) = (img.width().max(1) as f64, img.height().max(1) as f64);
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
    rx: &Mutex<Receiver<Job>>,
    events: &Sender<Event>,
    client: &reqwest::blocking::Client,
    picker: &Picker,
    cache_dir: &Path,
) {
    loop {
        let job = {
            let Ok(rx) = rx.lock() else { return };
            match rx.recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        let event = match job {
            Job::Fetch(url) => {
                let img = load(client, cache_dir, &url);
                Event::ImageDecoded(url, img)
            }
            Job::Encode { url, image, size } => {
                let font = picker.font_size();
                let px_w = u32::from(size.width) * u32::from(font.width);
                let px_h = u32::from(size.height) * u32::from(font.height);
                let scaled = image.resize(px_w, px_h, FilterType::CatmullRom);
                let proto = SlicedProtocol::new(picker, scaled, Some(size)).ok();
                Event::ImageEncoded(url, size, proto)
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
        img.resize(MAX_DECODED_WIDTH, u32::MAX, FilterType::Triangle)
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
