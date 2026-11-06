# Grimoir

A PDF-first document reader for Linux, built with Rust and GTK4.

Grimoir is designed around a simple idea: the book should be perfectly legible,
while everything around it can become transparent or frosted glass. It also has
a public, generic core and an optional local layer that adapts the app to the
desktop theme.

## Status

Phase 3 core is complete on the host. The app opens into a **library
showcase**: a transparent, glass-framed grid of book covers with a 2.5D tilt,
live search, section filters, favorites, collections, metadata editing,
series/folder browsing, scan progress and "continue reading" state. PDF, CBZ and
EPUB files are indexed into SQLite in the background, with cover extraction,
metadata, file watching, and persisted reading progress and per-book preferences.

The PDF reader uses PDFium in a transparent fullscreen GTK4 window, with
single/double pages, RTL/LTR, fit modes, zoom and pan, continuous vertical
scrolling, a table of contents, optional page-turn motion, asynchronous
rendering, and a page cache/prefetch pipeline. It also includes in-book search
with highlighted hits, region highlights, an annotations panel, Obsidian export
(page image + highlights appended to a note), and reading statistics.

CBZ volumes have a dedicated image reader. EPUB files currently use a readable
text fallback (no CSS/images) because WebKitGTK development headers are not
available in this environment; a full WebKit-based EPUB renderer and OCR remain
future work. Automatic margin cropping is also still pending. The library and
readers share the desktop theme.

## Performance

- The library uses a virtualized `GtkGridView`; only visible cards exist, covers
  are decoded lazily when mapped, and a bounded texture cache prevents repeated
  PNG decoding while scrolling.
- PDF pages render on a dedicated worker thread. Visible page turns go into an
  urgent queue, while neighbouring pages are prefetched in a normal queue.
- Rendered pages use bounded raw and texture caches. Neighbouring spreads are
  warmed in the background so a page turn does not need to filter and upload a
  new texture synchronously.
- The ink-on-glass filter is parallelized per scanline with Rayon.
- The library watcher ignores file-access events, so reading PDFs for metadata
  cannot trigger a scan loop. Refresh signatures avoid rebuilding the grid when
  nothing visible changed.

## Requirements

- Rust 1.92 or newer
- GTK 4 development files (`gtk4`, `pkg-config`)
- `curl` and `tar`
- Linux with Wayland or X11

For real backdrop blur, GTK 4.24+ and a compositor that supports
`ext_background_effect_manager_v1` (Hyprland does). Older GTK/compositors fall
back to alpha blending.

## Build and run

```sh
./scripts/fetch-pdfium.sh
cargo run -p grimoir
```

That opens the library. Add a folder with the folder button in the floating
toolbar; Grimoir indexes PDFs read-only and never moves or renames files.

To open a single PDF directly in the reader:

```sh
cargo run -p grimoir -- /path/to/book.pdf
```

PDFium is downloaded into `vendor/pdfium/`, which is intentionally ignored by
git. The binary is BSD-licensed; see `vendor/pdfium/LICENSE`.

To build a self-contained Linux tarball (binary, `libpdfium.so`, desktop entry,
icon and license):

```sh
./scripts/package.sh
```

The result is written to `dist/grimoir-linux-x86_64.tar.gz`.

While reading: `Left`/`Right` or `Page Up`/`Page Down` turn pages, `Home`/`End`
jump to the first/last page, `G` toggles ink-on-glass, `D` toggles double-page
spreads, `R` toggles right-to-left order, `S` toggles continuous scrolling, and
`M` toggles page-turn motion. `+`/`-`/`0` zoom in, out, and reset;
`Ctrl`+mouse-wheel also zooms. `Ctrl+F` opens in-book search, `A` toggles region
annotation mode, and `N` opens the annotations panel. `F`/`F11` toggles
fullscreen, and `Esc` closes the reader. The floating bar hides after a moment
and reappears when the pointer reaches the top edge.

## Desktop integration

The repository ships a desktop entry and a scalable icon in `assets/`. To make
Grimoir available from the desktop application menu or a launcher such as
Quickshell, install the release binary and the metadata:

```sh
cargo build --release -p grimoir

install -Dm755 target/release/grimoir ~/.local/bin/grimoir
install -Dm644 assets/grimoir.desktop ~/.local/share/applications/grimoir.desktop
install -Dm644 assets/grimoir.svg \
  ~/.local/share/icons/hicolor/scalable/apps/grimoir.svg

update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
```

If your launcher keeps an application index, refresh it afterwards. Quickshell
uses `qs ipc call launcher refresh`; Grimoir is then available by typing its name
after opening the launcher. `~/.local/bin` must be in `PATH` for the generic
`Exec=grimoir` entry; otherwise edit the copied `.desktop` file to use the
absolute path.

## Theme

The app looks for a theme in this order:

1. `$GRIMOIR_THEME`
2. `$XDG_CONFIG_HOME/grimoir/theme.json` (or `~/.config/grimoir/theme.json`)
3. `$GRIMOIR_HYPR_THEME` (or `~/.local/state/hypr/theme.json`)

The third fallback makes the app follow a Hyprland `theme-apply` setup during
development. The file is monitored and the CSS is reloaded on changes.

`assets/theme.example.json` documents the public theme schema. The personal
layer (not part of this repository) is responsible for generating this file
from the desktop's actual theme.

## License

GPL-3.0-or-later. See `LICENSE`.
