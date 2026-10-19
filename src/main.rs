// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Feedatui: a terminal feed reader with vim-style keys and inline images.

mod app;
mod article;
mod config;
mod feed;
mod images;
mod store;
mod theme;
mod ui;

use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, KeyEventKind};
use crossterm::execute;
use ratatui_image::picker::Picker;

use crate::app::{App, Scope};
use crate::config::{Config, Paths};
use crate::images::Images;

fn main() {
    if let Err(e) = run() {
        eprintln!("feedatui: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let paths = Paths::resolve()?;
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("feedatui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--paths" => {
                println!("config: {}", paths.config_file.display());
                println!("state:  {}", paths.state_file.display());
                println!("images: {}", paths.image_cache.display());
                return Ok(());
            }
            _ => {
                println!("usage: feedatui [--version | --paths]");
                println!(
                    "Feeds and groups are set in {}",
                    paths.config_file.display()
                );
                return Ok(());
            }
        }
    }
    let cfg = Config::load(&paths.config_file)?;

    if cfg.images {
        let dir = paths.image_cache.clone();
        let limit = cfg.cache_limit_mb;
        std::thread::spawn(move || images::prune_cache(&dir, limit));
    }

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        // The graphics probe talks to the terminal, so it runs after init.
        let picker = cfg.images.then(|| {
            let mut p = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
            p.set_background_color(Some(image::Rgba([0x28, 0x28, 0x28, 0xff])));
            p
        });
        execute!(std::io::stdout(), EnableMouseCapture)?;

        let (tx, rx) = channel();
        let images = picker.map(|p| Images::new(p, paths.image_cache.clone(), tx.clone()));
        let mut app = App::new(cfg, paths, images, tx);
        app.refresh(Scope::All);

        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(80))? {
                // Handle everything queued so fast key repeats don't lag.
                loop {
                    match event::read()? {
                        event::Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                        event::Event::Mouse(m) => app.on_mouse(m),
                        _ => {}
                    }
                    if app.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
            while let Ok(ev) = rx.try_recv() {
                app.on_event(ev);
            }
            app.auto_refresh();
        }
        app.save();
        Ok(())
    })();
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
