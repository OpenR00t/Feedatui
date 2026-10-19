# Feedatui

A terminal feed reader for RSS, Atom and JSON Feed, built with
[ratatui](https://ratatui.rs). Titles on the left, the full article on the
right, with the article's images drawn inline: real pixels in terminals that
speak sixel, the Kitty graphics protocol or iTerm2's inline images (Windows
Terminal, WezTerm, kitty, foot, Ghostty, iTerm2), half-block art everywhere
else.

- Feed groups, each with any number of feeds, picked from a popup (`b`); a
  group can be opened to read just one of its feeds
- Vim-style keys
- Articles render with headings, bold/italic, links, lists, quotes, code
  blocks and image captions
- Read state and fetched articles are kept between runs, so the list shows
  instantly and keeps articles a feed has since dropped
- Images are cached on disk, with a size limit

## Install

Needs Rust (2024 edition).

```sh
cargo install --path .
feedatui
```

The first run writes a commented `config.toml` with a few example feeds.
`feedatui --paths` prints where the config, state and image cache live.

## Configuration

```toml
refresh_minutes = 30   # 0 turns automatic refresh off
images = true          # false: no graphics probe, no image downloads
cache_limit_mb = 200   # image cache size
# browser = "firefox"  # program for links; default is the system browser

[[group]]
name = "Tech"
feeds = [
  { name = "Ars Technica", url = "https://feeds.arstechnica.com/arstechnica/index" },
  { url = "https://hnrss.org/frontpage" },   # name is optional
]

[[group]]
name = "Comics"
feeds = [{ url = "https://xkcd.com/atom.xml" }]
```

A feed may sit in more than one group. Setting `FEEDATUI_HOME` keeps the
config, state and cache together in that one folder.

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
| `b` | feed group picker: `j`/`k` move, `l`/`h` show or hide a group's feeds, `Enter` picks, `0`–`9` jump |
| `gt` / `gT` | next / previous group |
| `r` / `R` | refresh this group / everything |
| `m` | toggle read |
| `x` | show unread only |
| `/` | filter by title or feed name (`Enter` keeps it, `Esc` clears) |
| `?` | help |
| `q` | quit |

The mouse wheel scrolls whichever pane it is over; clicking a title selects
it.

An article is marked read when you move onto it, open its pane or open its
link. Refreshing or switching groups never marks anything read.

## How images work

Each article is laid out as text lines and image blocks. An image gets as
many rows as its aspect ratio needs at the pane's width (never wider than its
natural size, never taller than the pane), so it scrolls row by row with the
text. Downloading, decoding and encoding for the terminal happen on worker
threads; the space is held while that runs so the text doesn't jump. Images
smaller than 8 pixels (tracking pixels) are left out. While a popup is open,
images are replaced by their placeholder so terminal graphics can't paint
over it.

The graphics protocol is detected at startup by
[ratatui-image](https://github.com/ratatui/ratatui-image). In a terminal that
doesn't answer the query (some tmux setups), startup waits briefly and the
first key press can be lost; `images = false` skips the query.

## License

Licensed under the GNU Affero General Public License v3.0 or later. See
[LICENSE](LICENSE).
