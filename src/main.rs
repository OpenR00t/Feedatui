// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 The Feedatui contributors

//! Feedatui: a terminal feed reader with vim-style keys and inline images.

mod app;
mod article;
mod config;
mod feed;
mod fulltext;
mod html;
mod images;
mod nextjs;
mod opml;
mod store;
mod theme;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui_image::picker::Picker;

use crate::app::{App, Scope};
use crate::config::{Config, Paths};
use crate::images::Images;

const USAGE: &str = "usage: feedatui [command]

  (none)               open the reader
  import <file.opml>   add the feeds in an OPML file to config.toml
  export [file.opml]   write every group and feed as OPML (to stdout without a file)
  paths                show where the config, state and caches live
  --version            print the version";

fn main() {
    if let Err(e) = run() {
        eprintln!("feedatui: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let paths = Paths::resolve()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--version" | "-V") => {
            println!("feedatui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("paths" | "--paths") => {
            println!("config:   {}", paths.config_file.display());
            println!("state:    {}", paths.state_file.display());
            println!("images:   {}", paths.image_cache.display());
            println!("articles: {}", paths.article_cache.display());
            return Ok(());
        }
        Some("import") => {
            let file = args.get(1).context("import needs an OPML file")?;
            let r = opml::import(&PathBuf::from(file), &paths.config_file)?;
            println!(
                "Added {} feed(s) to {}{}.",
                r.added,
                paths.config_file.display(),
                if r.skipped > 0 {
                    format!(" ({} already there)", r.skipped)
                } else {
                    String::new()
                }
            );
            if !r.new_groups.is_empty() {
                println!("New groups: {}", r.new_groups.join(", "));
            }
            if r.added > 0 {
                println!("The previous config was kept as config.toml.bak.");
            }
            return Ok(());
        }
        Some("export") => {
            let cfg = Config::load(&paths.config_file)?;
            let out = opml::export(&cfg);
            match args.get(1) {
                Some(file) => {
                    std::fs::write(file, out).with_context(|| format!("writing {file}"))?;
                    println!("Wrote {file}");
                }
                None => print!("{out}"),
            }
            return Ok(());
        }
        Some(_) => {
            println!("{USAGE}");
            return Ok(());
        }
    }
    let cfg = Config::load(&paths.config_file)?;

    {
        let images = cfg.images.then(|| paths.image_cache.clone());
        let articles = paths.article_cache.clone();
        let limit = cfg.cache_limit_mb;
        std::thread::spawn(move || {
            if let Some(dir) = images {
                images::prune_cache(&dir, limit);
            }
            fulltext::prune_cache(&articles);
        });
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

        let mut dirty = true;
        let mut last_draw = Instant::now();
        while !app.quit {
            // Redraw on change, and once a second for clocks and messages.
            if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
                // Synchronized output: the terminal shows the frame at once,
                // so images don't flash while the screen is rewritten.
                execute!(std::io::stdout(), BeginSynchronizedUpdate)?;
                let drawn = terminal.draw(|f| ui::draw(f, &mut app));
                execute!(std::io::stdout(), EndSynchronizedUpdate)?;
                drawn?;
                dirty = false;
                last_draw = Instant::now();
            }
            let animating = app.animate();
            dirty |= animating;
            let wait = if animating { 8 } else { 25 };
            if event::poll(Duration::from_millis(wait))? {
                // Handle everything queued so a held key doesn't lag.
                loop {
                    match event::read()? {
                        event::Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                        event::Event::Mouse(m) => app.on_mouse(m),
                        _ => {}
                    }
                    dirty = true;
                    if app.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
            while let Ok(ev) = rx.try_recv() {
                app.on_event(ev);
                dirty = true;
            }
            if app.settle_due() {
                dirty = true;
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
