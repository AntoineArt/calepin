<p align="center">
  <img src="assets/icon.png" width="128" height="128" alt="Calepin logo">
</p>

<h1 align="center">Calepin</h1>

<p align="center">
  A fast, native notepad for Linux: tabs that are never lost, Markdown rendered
  as you type, tables, a minimap.<br>
  Built for quick notes, reading and editing, not as an IDE. GTK4 +
  GtkSourceView 5, written in Rust, MIT licensed.
</p>

<p align="center">
  <img src="https://github.com/AntoineArt/calepin/releases/download/media/calepin-showcase.webp"
       alt="Calepin showcase: Markdown rendering as you type, tables, search with minimap, live theme switching, tabs"
       width="800">
  <br>
  <sub><a href="https://github.com/AntoineArt/calepin/releases/download/media/calepin-showcase.mp4">Watch in MP4</a></sub>
</p>

It follows the [Omarchy](https://omarchy.org) theme and font live, and runs on
any recent Linux desktop.

## Install

### Flatpak (any distribution)

```bash
flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08 org.flatpak.Builder
flatpak run org.flatpak.Builder --user --install --force-clean build-dir \
  flatpak/io.github.antoineart.Calepin.yml
flatpak run io.github.antoineart.Calepin
```

The Flatpak brings its own GTK and GtkSourceView, so it also runs on older
distributions (Debian 12, Ubuntu 22.04).

### From source

Requirements: GTK ≥ 4.14, GtkSourceView ≥ 5.10 (Arch, Fedora ≥ 40,
Ubuntu ≥ 24.04…) and Rust ≥ 1.88 ([rustup](https://rustup.rs) if your
distribution ships an older one).

```bash
# Arch:          sudo pacman -S gtk4 gtksourceview5 rust
# Fedora:        sudo dnf install gtk4-devel gtksourceview5-devel
# Debian/Ubuntu: sudo apt install build-essential libgtk-4-dev libgtksourceview-5-dev
./install.sh   # binary in ~/.local/bin, desktop entry ("Open With"), icon
```

### Updates

At start-up, Calepin checks once for a newer version in the
[releases](https://github.com/AntoineArt/calepin/releases). When installed with
`install.sh` (binary in `~/.local/bin`), it downloads the new binary, verifies
its SHA-256, swaps it in, then offers to restart (your tabs are kept). As a
Flatpak or a system package, it only tells you a new version is out.
`CALEPIN_NO_UPDATE=1` turns the check off.

To publish a version: bump `version` in `Cargo.toml`, then push a `vX.Y.Z`
tag; the `release.yml` workflow builds the binary and creates the release.

## Usage

- `calepin` opens the window, `calepin file.md` opens the file in the running
  instance (single instance), `calepin --new` opens an empty tab,
  `calepin --background` loads everything without showing a window (for
  session autostart), `calepin --version` prints the version.
- Middle click: fast scrolling (move away from where you clicked, click
  again or Escape to stop; or hold the button). Right-click → "Middle click:
  fast scroll" turns it off, and the middle click pastes again.
- Closing the window only hides it: the process stays resident so it shows up
  instantly next time. Ctrl+Q really quits.
- Every tab is saved continuously (`~/.local/state/calepin/`), even a note
  never saved to a file: closing the window, shutting down or killing the
  process loses nothing. Ctrl+Shift+T reopens the last 30 closed tabs.
- Tabs: nothing for a note never saved to a file; a file icon for a saved
  file; the icon in the accent colour plus ● for a modified, unsaved file.
  Closing an unsaved note asks for confirmation.
- `.md` files open in rich mode, any other file as plain text with syntax
  highlighting. A binary or non-UTF-8 file opens read-only.
- Folding: Markdown sections (a heading and what follows, up to the next
  heading of the same level), JSON objects and arrays, YAML blocks. Click the
  arrow left of the line (shown on hover) or the "⋯" after a folded line;
  Ctrl+Alt+← / → (or Ctrl+Shift+[ / ]) folds or unfolds at the caret,
  Ctrl+Alt+Shift+← / → everything. Folded text stays in the file, is copied
  and searched; a search hit or an edit inside unfolds it. Folds are kept
  with the tab across restarts.
- `.csv` and `.tsv` files open as an editable table (the first row is the
  header; the separator, `,` `;` tab or `|`, is detected) and are saved back
  as CSV, with their separator and line endings. A file left untouched is
  written back byte for byte. The CSV/Table button (Ctrl+Shift+E) shows the
  raw text. A field spanning several lines, an unclosed quote or more than
  20,000 cells keeps the file in plain text.
- Drag a tab out of the tab bar (onto the text or outside the window) to open
  it in its own window; drop it on a tab bar to move or reorder it.
  Ctrl+Shift+D from the keyboard.
- Theme: on Omarchy, the current theme's colours and monospace font, followed
  live. Elsewhere, the desktop's light or dark style and accent colour (XDG
  portal), followed live too.
- Title bar: none under a tiling window manager (Hyprland, Sway, niri, i3…);
  elsewhere the tab bar doubles as the title bar. `CALEPIN_DECORATIONS=1` or
  `0` forces either.
- Language: English, or French when your locale is French (`LANG`,
  `LC_MESSAGES`…). The translations are built into the binary.
- Window titles: the main window's title starts with "Calepin", detached
  windows' titles end with "— Calepin", so window manager rules can tell them
  apart.

## Markdown input

Syntax disappears as soon as it is complete: `# ` `## ` `### ` headings,
`- ` bullets, `1. ` numbered list, `[] ` checkbox, `> ` quote,
` ``` ` + Enter code block, `**bold**`, `*italic*`, `~~strikethrough~~`,
`` `code` ``, `[text](url)`, bare URLs.

Tables: `| a | b |`, Enter, `|---|---|`, Enter (or the ▦ button,
Ctrl+Alt+T). Drag across cells (or Shift+click, Shift+arrows) to select
several: Ctrl+C copies them as tab-separated text, Ctrl+X and Delete clear
them, and tab-separated text pasted into a cell fills the cells from there.
Drag a column's border to resize it (kept with the tab). A table wider than
its room scrolls sideways: Shift+wheel, a sideways swipe or its scrollbar. Tab and Shift+Tab move between cells, Tab on the last cell adds a
row, Escape leaves the table. Cell text wraps; "⇔ Widen" lets a table extend
past the text column.

Enter on an empty list item leaves the list; Backspace at the start of a
heading or quote turns it back into text; Tab and Shift+Tab indent lists.
Click a checkbox to tick it, Ctrl+click to open a link. Copy and paste go
through Markdown; Ctrl+Shift+V pastes plain text. Ctrl+Z undoes text and
formatting together (the history keeps the Markdown), table edits included,
a word at a time; right after an automatic format (`**bold**`, `- `), it first
gives back what you typed. The undo history of Markdown and CSV tabs survives
a restart.

## Shortcuts

| Action | Keys |
|---|---|
| Bold, italic, strikethrough, code | Ctrl+B, Ctrl+I, Ctrl+Shift+X, Ctrl+E |
| Link | Ctrl+K |
| Text, headings 1-3 | Ctrl+Alt+0, Ctrl+Alt+1..3 |
| Checkbox, bullets, numbers, code, quote | Ctrl+Alt+4, 5, 6, 8, Ctrl+Alt+Q |
| Table | Ctrl+Alt+T |
| Clear formatting | Ctrl+\ |
| Undo, redo | Ctrl+Z, Ctrl+Shift+Z (or Ctrl+Y) |
| New tab, close, reopen | Ctrl+T (or Ctrl+N), Ctrl+W, Ctrl+Shift+T |
| Detach tab | Ctrl+Shift+D (or drag the tab out of the bar) |
| Next / previous tab | Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+PgDown/PgUp, Alt+1..9 |
| Move tab | Ctrl+Shift+PgDown/PgUp |
| Open, save, save as | Ctrl+O, Ctrl+S, Ctrl+Shift+S |
| Find (ignores case and accents) | Ctrl+F, Enter / Shift+Enter, F3, Escape |
| Minimap, toolbar, Markdown source | Ctrl+M, Ctrl+Shift+F, Ctrl+Shift+E |
| Fold, unfold (at the caret) | Ctrl+Alt+←, Ctrl+Alt+→ (or Ctrl+Shift+[, ]) |
| Fold all, unfold all | Ctrl+Alt+Shift+←, Ctrl+Alt+Shift+→ |
| Zoom (remembered) | Ctrl+scroll, Ctrl++, Ctrl+-, Ctrl+0 |
| Quit | Ctrl+Q |

## Development

```bash
cargo test                  # Markdown model, search
cargo build --release
CALEPIN_TIMING=1 calepin    # start-up timings
```

After a dependency change, regenerate `flatpak/cargo-sources.json` with
[`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).

## License

[MIT](LICENSE)
