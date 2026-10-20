# Feedatui

A terminal feed reader for RSS, Atom and JSON Feed, built with
[ratatui](https://ratatui.rs). Titles on the left, the full article on the
right, with the article's images drawn inline: real pixels in terminals that
speak sixel, the Kitty graphics protocol or iTerm2's inline images (Windows
Terminal, WezTerm, kitty, foot, Ghostty, iTerm2), half-block art everywhere
else.

- **Full articles** for feeds that only send a teaser: the article's page is
  fetched and the article pulled out of it (Mozilla's Readability algorithm)
- The article's first picture sits in the top-left corner with the text
  wrapped around it, magazine style
- Feed groups, each with any number of feeds, picked from a popup (`b`) or a
  tab row with unread counts
- Feeds are kept in a plain `config.toml`; OPML import and export for moving
  between readers
- Vim-style keys
- Articles render with headings, bold/italic, links, lists, quotes, code
  blocks and image captions
- Read state and fetched articles are kept between runs

## Install

Needs Rust (2024 edition).

```sh
cargo install --path .
feedatui
```

The first run writes a commented `config.toml` with some example feeds.

| Command | |
|---|---|
| `feedatui` | open the reader |
| `feedatui import subscriptions.opml` | add the feeds from an OPML file to `config.toml` (OPML folders become groups; feeds already in a group are skipped; the old file is kept as `config.toml.bak`) |
| `feedatui export [file.opml]` | write all groups and feeds as OPML |
| `feedatui paths` | show where the config, state and caches live |

## Configuration

```toml
refresh_minutes = 30    # 0 turns automatic refresh off
full_article = "auto"   # "auto", "always" or "never"
images = true           # false: no graphics probe, no image downloads
lead_image_width = 40   # % of the pane for the top-left picture (0: above the text)
image_max_height = 40   # tallest image, % of the pane height
smooth_scroll = true    # glide over page jumps instead of snapping
terminal_title = true   # tab title: "Feedatui · Tech · 12 unread"
cache_limit_mb = 200    # image cache size
# browser = "firefox"   # program for links; default is the system browser

[[group]]
name = "Tech"
feeds = [
  { name = "Rust Blog", url = "https://blog.rust-lang.org/feed.xml" },
  { url = "https://feeds.arstechnica.com/arstechnica/index" },   # name is optional
]

[[group]]
name = "Comics"
full_article = "never"   # per group…
feeds = [
  { url = "https://xkcd.com/atom.xml" },
  # { url = "…", full_article = "always" },   # …or per feed
]
```

A feed may sit in more than one group. Setting `FEEDATUI_HOME` keeps the
config, state and caches together in that one folder.

### Full articles

With `"auto"`, an article is fetched from its web page when the feed's text
looks like a teaser: under about 250 words, or ending in "Read more",
"Continue reading" or `[…]`. `"always"` fetches every article, `"never"`
shows only the feed's text. Whatever the setting, `f` switches the article
on screen between the two. A line under the article's link says which one
is showing.

Fetched articles are cached for two weeks. The next two articles in the list
are fetched in the background, so moving down the list rarely waits.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | next / previous title, or scroll the article |
| `h` / `l`, `←` / `→` | focus the list / the article (also `Tab`, `Enter`) |
| `J` / `K` | next / previous article from either pane |
| `gg` / `G` | top / bottom |
| `Ctrl-d` / `Ctrl-u` | half page down / up |
| `Ctrl-f` / `Ctrl-b`, `Space` | page down / up |
| `o` | open the article's link in the browser |
| `f` | full article / feed text |
| `b` | feed group picker: `j`/`k` move, `l`/`h` show or hide a group's feeds, `Enter` picks, `0`–`9` jump |
| `gt` / `gT` | next / previous group |
| `r` / `R` | refresh this group / everything |
| `m` | toggle read |
| `x` | show unread only |
| `/` | filter by title or feed name (`Enter` keeps it, `Esc` clears) |
| `?` | help |
| `q` | quit |

The mouse wheel scrolls whichever pane it is over; clicking a title selects
it. Half-page, page, top/bottom and mouse-wheel jumps in the article glide
over a few frames (about a tenth of a second) rather than snapping; single
lines move at once. `smooth_scroll = false` turns the glide off.

An article is marked read when you move onto it, open its pane or open its
link. Refreshing or switching groups never marks anything read.

## How images work

Each article is laid out as text and image blocks. The first picture (or
the feed's own image for the article) is placed in the top-left corner,
`lead_image_width` percent of the pane wide, with the title and text beside
it; on narrow panes it goes above the text instead. Other images are
centered, never wider than their natural size and never taller than
`image_max_height` percent of the pane, with their alt text as a caption.

Where a page offers several sizes (`srcset`), a copy about 1000 pixels wide
is downloaded rather than the original. Downloading, decoding and encoding
run on worker threads, the article on screen first, then the images of the
next articles.

Frames are drawn with synchronized output, so terminals that support it
(including Windows Terminal) show each frame whole instead of flashing.
Sixel and iTerm2 images are large to send, so while a key is held to scroll
a half-block copy stands in, and the sharp image is sent once scrolling
pauses. While a popup is open, images are replaced by their placeholder so
terminal graphics can't paint over it.

The graphics protocol is detected at startup by
[ratatui-image](https://github.com/ratatui/ratatui-image). In a terminal that
doesn't answer the query (some tmux setups), startup waits briefly and the
first key press can be lost; `images = false` skips the query.

## License

Licensed under the GNU Affero General Public License v3.0 or later. See
[LICENSE](LICENSE).
