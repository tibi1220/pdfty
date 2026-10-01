# pdfty

A PDF viewer for Kitty, with optional tmux support and Neovim SyncTeX integration.

## Requirements

macOS, Kitty, Rust, `mutool`, `pdfinfo`, and `kitten` on `PATH`. SyncTeX mode
also requires `synctex` and `nvr`.
tmux is optional; run the viewer directly in a Kitty pane or inside tmux.
Compile LaTeX with `-synctex=1`.

```sh
cargo install --path . --force
pdfty
```

Cargo builds in release mode and installs `pdfty` into `~/.cargo/bin`
(or your configured Cargo install root). Ensure that directory is on `PATH`.
Run the install command again after updating the source.

The no-argument viewer opens blank and waits for Neovim. Open a TeX file in
Neovim and run `:PdftyAdd` to add its compiled PDF. Repeat from other TeX
buffers to keep multiple PDFs in the same viewer. `:PdftyForward` switches to
the matching PDF before jumping to its source location. `[` and `]` switch PDFs
manually. You can also open several PDFs at startup:

```sh
pdfty first.pdf second.pdf --synctex
```

Each PDF socket name is derived from its canonical absolute path. Use
`pdfty document.pdf --get-socket` to print it, or `pdfty --list` (`-l`) to list
active sockets and their PDFs. A line without a PDF is the viewer's control
socket for adding documents. The no-argument viewer enables SyncTeX IPC; when
starting with PDF arguments, pass `--synctex` to enable it.

## Neovim setup with lazy.nvim

Add this spec to your Lazy plugin list. Replace `dir` with your checkout path.
Lazy's build step installs the executable globally with Cargo; run `:Lazy build pdfty`
after updating the Rust source.

```lua
{
  dir = vim.fn.expand("~/path/to/pdfty"),
  name = "pdfty",
  main = "pdfty",
  ft = { "tex" },
  cmd = {
    "PdftyAdd", "PdftyRegister", "PdftyForward", "PdftyMove",
    "PdftyNext", "PdftyPrev", "PdftyFirst", "PdftyLast",
    "PdftyZoom", "PdftyView", "PdftyReload", "PdftySwitch",
    "PdftyNextPdf", "PdftyPrevPdf", "PdftyCenter", "PdftyClear",
  },
  build = "cargo install --path . --force",
  opts = {
    command = "pdfty",
    auto_register = true,
    register_interval = 5000,
    notifications = true,
    keymaps = {
      add = "<leader>pa",
      forward = "<leader>pf",
      next = "<leader>pj",
      prev = "<leader>pk",
      move = "<leader>pm",
      switch = "<leader>pp",
      zoom_in = "<leader>p+",
      zoom_out = "<leader>p-",
      view = "<leader>pt",
      center = "<leader>pc",
      clear = "<leader>ph",
    },
  },
}
```

Start `pdfty` in another Kitty or tmux pane, open a TeX file, then use
`<leader>pa` to add its compiled PDF and `<leader>pf` for forward search.
With the mappings above, `3<leader>pj` advances three pages and
`10<leader>pm` jumps to page 10. `<leader>` is your Neovim leader key.
The plugin installs no mappings by default; only entries in `keymaps` are mapped.
These mappings become available after the plugin loads, by opening a TeX buffer
or running one of its commands.

### Configuration

Without Lazy, add this checkout to `runtimepath` and call
`require("pdfty").setup({...})`. Calling `setup` again replaces the configuration,
registration hooks, timer, and plugin mappings.

| Option | Default | Purpose |
| --- | --- | --- |
| `command` | `"pdfty"` | Executable name on `PATH`, or an absolute executable path. |
| `auto_register` | `true` | Automatically pair TeX buffers with active viewers. |
| `register_interval` | `5000` | Polling interval in milliseconds; `0` disables polling. |
| `register_events` | `{ "BufEnter", "BufWritePost", "FocusLost" }` | Events that refresh pairing; `{}` disables event registration. |
| `notifications` | `true` | Display plugin notifications, including errors. |
| `resolve_pdf` | `nil` | Optional `function(bufnr)` returning a PDF path, or `nil` to use automatic lookup. |
| `keymaps` | `{}` | Normal-mode mappings by action; omit an action or use `false` to leave it unmapped. |

Mapping actions: `add`, `register`, `forward`, `move`, `next`, `prev`, `first`,
`last`, `switch`, `next_pdf`, `prev_pdf`, `zoom_in`, `zoom_out`, `zoom_reset`,
`view`, `reload`, `center`, and `clear`. `move`, `next`, and `prev` use the Neovim numeric count.
For compatibility, `vim.g.pdfty_command` supplies the executable when no
`command` option is passed.

The plugin discovers active `pdfty --synctex` viewers and registers Neovim
automatically. With VimTeX it uses the compiled PDF path, including a custom
output directory; otherwise SyncTeX finds the viewer containing the current
source. A custom `resolve_pdf` takes precedence over VimTeX. No socket setting
is needed.

`:PdftyAdd` uses VimTeX's compiled PDF path and accepts an optional PDF path.
Without VimTeX, it checks for a PDF beside the current TeX file. `:PdftyForward`
jumps from the current source line; `:PdftyRegister` manually refreshes the
pairing. Both commands accept an optional PDF path. When SyncTeX lands on a text paragraph, the viewer
outlines that paragraph in red in either view. The outline also appears after a
successful inverse search.

### Neovim navigation commands

All commands now start with `Pdfty`; update any mappings that used `PdfView`.
Page and view commands target the PDF paired with the current TeX buffer and
show that PDF first. With one open PDF they also work from other buffer types.
PDF cycling acts on the viewer's currently displayed document.

| Command | Action |
| --- | --- |
| `:PdftyAdd [pdf]` | Add a compiled PDF to the viewer. |
| `:PdftyRegister [pdf]` | Pair the current TeX buffer with a viewer. |
| `:PdftyForward [pdf]` | Show the paragraph at the source cursor. |
| `:PdftyMove <page>` | Jump to an absolute page, numbered from 1. |
| `:PdftyNext [count]` | Advance by one page, or by the given count. |
| `:PdftyPrev [count]` | Go back by one page, or by the given count. |
| `:PdftyFirst` / `:PdftyLast` | Jump to the first / last page. |
| `:PdftyZoom <percent\|in\|out\|reset>` | Set zoom to 50–300%, adjust by 25%, or reset zoom and pan. |
| `:PdftyView [single\|scroll\|toggle]` | Set the view mode; no argument toggles it. |
| `:PdftyReload` | Reload the PDF from disk. |
| `:PdftyCenter` | Pan/scroll so the point under the cursor is centered; preserve its document position and zoom. |
| `:PdftyClear` | Remove text selection and forward/inverse search outlines. |
| `:PdftySwitch [pdf]` | Choose an open PDF, or switch to the given path. |
| `:PdftyNextPdf` / `:PdftyPrevPdf` | Cycle through PDFs in the viewer. |

Relative page movement stops at the first or last page. An absolute page outside
the document's range reports an error in the viewer. Zoom and view arguments
have command completion.

Example mappings:

```lua
vim.keymap.set("n", "<leader>pf", "<Cmd>PdftyForward<CR>")
vim.keymap.set("n", "<leader>pj", "<Cmd>PdftyNext<CR>")
vim.keymap.set("n", "<leader>pk", "<Cmd>PdftyPrev<CR>")
vim.keymap.set("n", "<leader>pp", "<Cmd>PdftySwitch<CR>")
```

Inverse search uses the server paired by the plugin, falling back to
`/tmp/vimtexserver.txt` when no editor is paired, and
opens the SyncTeX source with `nvr --nostart --servername <server> --remote-solent <file>
-c <line>`. Write the active server address to that file from Neovim, for example
with `vim.fn.writefile({vim.v.servername}, '/tmp/vimtexserver.txt')`.

## Viewer keys and examples

Type a number before a movement key to repeat it. Counts work with
`w`/`a`/`s`/`d`, page and scroll navigation, `H`/`J`/`K`/`L` panning,
zoom (`=`/`+`/`-`), and PDF switching (`[`/`]`). Counts are consumed by the
next key. Backspace edits a pending count; Escape cancels it. Bare `0` still
resets zoom. Page movement stops at the document's ends; cursor movement stays
inside the viewport and pans or scrolls when it reaches an edge.

| Keys | Action |
| --- | --- |
| `10w` | Move the cursor up ten cells. |
| `5d` | Move the cursor right five cells. |
| `3j` / `3k` | Move three pages in single view, or scroll nine rows in continuous view. |
| `2=` | Zoom in by 50 percentage points. |
| `4H` | Pan left sixteen cells. |
| `10g`, `10G`, or `10` then Enter | Jump to page 10. |
| `v`, `5s`, `y` | Anchor selection at the cursor, extend down five cells, then copy. |
| `c` | Center the document point under the cursor. |
| Shift+I (`I`) | Run inverse search and focus the paired Neovim pane/window. |
| Escape | Remove all selection/search highlights and cancel a pending count. |
| `t` / `0` / `?` / `q` | Toggle view / reset zoom / show help / quit. |

Counts do not repeat selection, copy, inverse search, reload, or view toggles.

Keys: `h`/`l` or arrow keys change pages, `[`/`]` switch PDFs, `g`/`G` jump to the ends, `r` reloads,
right click or `i` runs inverse search, Shift+I also focuses Neovim, and `q` quits. Press `?` for the full
shortcut menu; use Up/Down or Page Up/Down to scroll it and `?` or Escape to close it.
Press `:`, type a page number,
then Enter to jump to that page. Backspace edits the number; Escape cancels the prompt.
Shift+I selects Neovim’s tmux session, window, and pane. For a separate Kitty
window, it uses Kitty remote control (`kitten @ focus-window`), which must be
enabled in Kitty. Plain `i`, Enter, and right-click keep focus in the viewer.

Press `v` to start
a keyboard selection at the cursor and use `w`/`a`/`s`/`d` to extend it; Escape clears all selection and search highlights.
Press `y` to copy the selected text. Selection currently snaps to whole PDF text lines.

Press `t` to switch between single-page and continuous scroll views. In scroll view,
`j`/`k`, the arrow keys, Page Up/Down, or the mouse wheel scroll through pages.
In either view, `=` zooms in, `-` zooms out, and `0` resets to 100%.
At high zoom, `H`/`L` pan horizontally; in single-page view, `J`/`K` pan vertically.
The cursor has a black block background and is always visible on the PDF, including when another pane
has focus. `w`/`a`/`s`/`d` move it up/left/down/right. At a viewport edge,
movement pans a zoomed page or scrolls continuous view so the cursor stays visible.
Press `v` to anchor selection at the cursor, then use the same keys to extend it.
The cursor turns blue during selection; Escape clears all highlights.
Press `c` (or `:PdftyCenter` from Neovim) to pan or scroll the document so
the point under the cursor appears at the screen center. The cursor stays on
the same document point, and zoom and selection stay unchanged. Blank margins
allow centering points near page edges. Use `:PdftyClear` from Neovim to remove selection and search highlights. A selection
stays within its starting page. Forward search moves the cursor to its target.
The mouse wheel pans vertically in single-page view when zoomed, and scrolls the document
in continuous view. A horizontal wheel gesture or Shift + vertical wheel pans horizontally
in either view when the page is wider than the pane.
All pages are rendered in the background after the first page appears. The top
bar shows the current page, zoom, prepared page count, status, and frame rate.
Three lines below the PDF keep the latest SyncTeX and `nvr` calls and their
results visible. The bottom bar shows the view mode, Neovim connection, and
socket name.
Rendering uses a high resolution image sized for the
pane width, including in scroll view. PNGs use 12 pixels per terminal column;
their pixel height follows the terminal's measured cell aspect ratio (24 pixels
when the terminal does not report it).
Scroll frames are composed in a background worker and displayed with `kitten icat`.
The viewer supplies the window dimensions and captures `icat`'s escape codes so
`icat` never queries the terminal or changes its input modes.
Each frame uses a fresh 24-bit image ID. Image placeholders and the search
marker are updated together using synchronized output. Focus changes reuse the
existing image. Inside tmux, the viewer sets `allow-passthrough=all` for its own
pane so image commands survive redraws, and restores the previous setting on exit.

The viewer creates a control socket, a socket per added PDF, and a temporary PNG
directory per process, and removes them on exit.

## Rust code organization

`src/app.rs` owns viewer state and the event loop. The modules in `src/app/`
separate input, geometry, cursor/navigation, document lifecycle, selection,
render scheduling, frame placement, status/help, and Neovim integration.

`src/image.rs` exposes the image API; `src/image/compose.rs` handles raster
composition/cache reuse, `overlays.rs` derives highlights, and `transport.rs`
emits Kitty image commands. `src/synctex/focus.rs` handles terminal focus after
a successful source jump. Module and method comments describe coordinate units,
state ownership, and how outdated background work is rejected.

## Testing with the sample

Build the LaTeX fixture after changing `sample/main.tex`, then run the Rust tests:

```sh
cd sample
latexmk -lualatex -synctex=1 -outdir=build main.tex
cd ..
cargo test --offline
nvim --headless -u NONE -l tests/nvim_commands.lua
```

The tests render `sample/build/main.pdf` and check forward and inverse SyncTeX
mapping. To try the Neovim workflow, start `pdfty` and open
`sample/main.tex` in Neovim with this repository on `runtimepath`. Run
`:PdftyAdd`; then right-click the PDF for inverse search or use
`:PdftyForward` for forward search.
