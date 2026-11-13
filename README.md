# pdfty

A fast PDF previewer for Kitty (directly or inside tmux), built for watching
`latexmk` output next to Neovim, with SyncTeX forward and inverse search.

## How it works

Everything runs in-process; no helper programs are spawned while viewing.

- **MuPDF is compiled in** (the `mupdf` crate builds it from source and links it
  statically). Pages render on a pool of worker threads, at exactly the
  terminal's cell pixel density, so Kitty never rescales them.
- **Pixels are sent once.** Pages are cut into cell-aligned tiles. Each tile is
  uploaded once with the Kitty graphics protocol and shown through Unicode
  placeholder characters. Scrolling only rewrites placeholder text, and a pure
  vertical scroll becomes a terminal scroll that redraws just the new rows. On
  our side a scroll frame takes well under a millisecond.
- **Visible tiles come first.** After every view change the worker queue is
  re-prioritised: visible tiles first, then page text, then one screen above and
  below. While a replacement renders (after a reload, zoom or highlight change),
  the previous image stays on screen, so nothing flashes.
- **SyncTeX is parsed natively** from `<name>.synctex.gz`, and Neovim is reached
  directly over its msgpack-RPC socket. `synctex`, `nvr`, `mutool`, `pdfinfo` and
  `kitten icat` are no longer needed.

## Requirements

- Kitty 0.28 or newer (Unicode placeholders).
- Optional: tmux 3.3 or newer. pdfty sets `allow-passthrough all` on its own pane
  and restores it on exit. tmux must pass 24-bit colour to Kitty, because image IDs
  are encoded in the foreground colour. If images don't appear inside tmux, add
  `set -as terminal-features ',xterm-kitty:RGB'` to your tmux config.
- Rust, plus a C compiler (Xcode Command Line Tools on macOS). The first build
  compiles MuPDF, which takes a minute.
- Compile LaTeX with `-synctex=1`.

```sh
cargo install --path . --force
pdfty                 # blank viewer, waits for Neovim's :PdftyAdd
pdfty thesis.pdf      # open a PDF directly (add --synctex for Neovim integration)
```

Pixels are passed to Kitty in a temporary file by default, which is fastest. Over
SSH (or with `PDFTY_TRANSFER=direct`) they are sent inline, zlib-compressed.

Licensing note: MuPDF is AGPL-3.0, and so is any `pdfty` binary that links it.

## Viewing

Colours follow the nightfox palette. The status line at the top shows the file, page, zoom, view mode, frames per
second, rendering progress (tiles ready around the view), the Neovim connection
and the latest message. Under the page, three debug lines show the newest log
entries: SyncTeX and Neovim calls, reloads and errors. `D` hides or shows them;
start with `--no-debug-lines` (or set `debug_lines = false` in the plugin) to
begin without them. Continuous scrolling
is the default view; `t` switches to single pages. The PDF reloads automatically
when `latexmk` rewrites it, keeping your scroll position.

| Keys | Action |
| --- | --- |
| `j`/`k`, `↓`/`↑`, wheel | Scroll 3 rows (single view: next/previous page) |
| `Ctrl-d`/`Ctrl-u` | Half a screen down/up |
| `Space`, `PgDn` / `PgUp` | A screen down/up |
| `h`/`l`, `←`/`→` | Previous/next page |
| `g`/`G`, `Home`/`End` | First/last page; `10g` goes to page 10 |
| `:10⏎` or `10⏎` | Go to page 10 |
| `H`/`L`, `J`/`K`, Shift+wheel | Pan left/right, down/up |
| `=`/`+`/`-` | Zoom in/out by 25% (25–500%), keeping the point under the cursor in place |
| `0` | Reset zoom |
| `t` | Toggle continuous / single-page view |
| `[`/`]` | Previous/next PDF |
| `r` | Reload |
| `w`/`a`/`s`/`d` | Move the cursor (scrolls at the edges) |
| `c` | Center the point under the cursor |
| Left click / drag | Move the cursor / select text |
| `v`, then move | Select text from the cursor (character precise) |
| `y` | Copy the selection (`pbcopy`, `wl-copy`, `xclip`, else OSC 52) |
| `i`, `Enter`, right click | Inverse search: open the source line in Neovim |
| `I`, Ctrl+click | Inverse search and focus Neovim's tmux pane / Kitty window |
| `Esc` | Clear selection, SyncTeX outline and pending count |
| `D` | Show/hide the debug lines under the page |
| `M` | Full message log, wrapped, never cut off (`j`/`k`, PgUp/PgDn, wheel scroll) |
| `?` | Help |
| `q` | Quit |

A number before a movement repeats it (`5j`, `3l`, `2=`). The cursor is the `+`
cell; it turns blue while selecting. Forward and inverse search outline the
target paragraph in red.

## Neovim

### Side panel

Like claudecode.nvim's Claude terminal, the plugin can run pdfty in a split
beside your code. `:Pdfty` toggles the panel and opens the current TeX file's
PDF. `:PdftyForward` jumps to the paragraph under your cursor and opens the
panel first if no viewer is running. Inverse search from the panel (`i`, Enter
or right click) shows the source in your last editor window, never in the panel
itself; `I` also moves focus there. While hidden, the viewer keeps running, so
showing it again is instant. It quits cleanly when Neovim exits.

Inside the panel you are in terminal mode, so keys go to the viewer, except
Ctrl-H/J/K/L. Those move to the neighbouring window, through
vim-tmux-navigator when it is installed, so they also cross tmux panes.
`<C-\><C-n>` returns to Normal mode, and `:PdftyFocus` (mapped below) jumps in
and out.

How images get through: Neovim's terminal emulator drops Kitty graphics
commands but keeps text and colours. So the viewer's placeholder text flows
through Neovim like any other output, while its image commands are handed to
Neovim with `nvim_ui_send` (Neovim 0.12+). Neovim writes them to the real
terminal between its own redraws, so they can't be torn apart. On older Neovim
the viewer writes to Neovim's terminal device directly (`PDFTY_TTY`, found via
`ps`); that works, but heavy redraws (rapid zooming) can occasionally garble a
command. This needs `termguicolors` (the plugin warns if it is off) and Kitty,
directly or through tmux. The cell size is read from Neovim's terminal device
(or from tmux), because Neovim's terminal doesn't report pixel sizes.

### Sockets

The viewer opens a Unix socket per PDF (named from its canonical path) and a
control socket for adding PDFs. `pdfty --list` (`-l`) prints the active sockets,
`pdfty doc.pdf --get-socket` prints one, and `pdfty --has-source doc.pdf file.tex`
exits 0 if the PDF was built from that source. The plugin uses these to pair TeX
buffers with viewers. The no-argument viewer always enables IPC; with PDF arguments,
pass `--synctex`.

### lazy.nvim

```lua
{
  dir = vim.fn.expand("~/path/to/pdfty"),
  name = "pdfty",
  main = "pdfty",
  ft = { "tex" },
  cmd = {
    "Pdfty", "PdftyFocus", "PdftyOpen", "PdftyClose", "PdftyAdd", "PdftyRegister", "PdftyForward", "PdftyMove",
    "PdftyNext", "PdftyPrev", "PdftyFirst", "PdftyLast",
    "PdftyZoom", "PdftyView", "PdftyReload", "PdftySwitch",
    "PdftyNextPdf", "PdftyPrevPdf", "PdftyCenter", "PdftyClear",
  },
  build = "cargo install --path . --force",
  opts = {
    terminal = {
      split_side = "right",
      split_width_percentage = 0.40,
    },
    keymaps = {
      toggle = "<leader>pv",
      focus = "<leader>po",
      add = "<leader>pa",
      forward = "<leader>pf",
      next = "<leader>pj",
      prev = "<leader>pk",
      switch = "<leader>pp",
    },
  },
}
```

Open a TeX file and press `<leader>pv` for the panel, or `<leader>pf` to jump
straight to the cursor's paragraph. A standalone `pdfty` in another tmux or Kitty
pane still works as before: `:PdftyAdd` sends PDFs to it while the panel is not
running. Inverse search from a standalone viewer opens the file in the paired
Neovim. If no editor is paired, it falls back to the server address in
`/tmp/vimtexserver.txt`.

Without Lazy, add the checkout to `runtimepath` and call `require("pdfty").setup({...})`.

| Option | Default | Purpose |
| --- | --- | --- |
| `command` | `"pdfty"` | Executable name or path (`vim.g.pdfty_command` also works). |
| `auto_register` | `true` | Pair TeX buffers with running viewers automatically. |
| `register_interval` | `5000` | Polling interval in ms; `0` disables polling. |
| `register_events` | `{ "BufEnter", "BufWritePost", "FocusLost" }` | Events that refresh pairing. |
| `notifications` | `true` | Show plugin notifications and errors. |
| `resolve_pdf` | `nil` | `function(bufnr)` returning the PDF path; overrides VimTeX lookup. |
| `keymaps` | `{}` | Normal-mode mappings by action (`false` leaves one unmapped). |
| `debug_lines` | `true` | Start the panel viewer with the three debug lines under the page (`D` toggles). |
| `terminal.split_side` | `"right"` | Panel side, `"left"` or `"right"`. |
| `terminal.split_width_percentage` | `0.40` | Panel width as a fraction of the editor. |
| `terminal.auto_insert` | `true` | Enter terminal mode when the panel is focused. |
| `terminal.auto_close` | `true` | Close the panel when the viewer exits (errors are always reported). |
| `terminal.navigation_keys` | `true` | Ctrl-H/J/K/L in the panel move to the neighbouring window. |
| `terminal.auto_open` | `true` | `:PdftyAdd` / `:PdftyForward` open the panel when no viewer runs. |

Mapping actions: `toggle`, `focus`, `add`, `register`, `forward`, `move`, `next`, `prev`, `first`,
`last`, `switch`, `next_pdf`, `prev_pdf`, `zoom_in`, `zoom_out`, `zoom_reset`,
`view`, `reload`, `center`, `clear`. `move`, `next` and `prev` take the count.

| Command | Action |
| --- | --- |
| `:Pdfty [pdf]` | Toggle the side panel. |
| `:PdftyFocus [pdf]` | Focus the panel, or hide it when it is focused. |
| `:PdftyOpen [pdf]` / `:PdftyClose` | Show (and add the PDF) / hide the panel. |
| `:PdftyAdd [pdf]` | Add the compiled PDF (VimTeX's, or the one beside the TeX file). |
| `:PdftyRegister [pdf]` | Pair the current buffer with a viewer. |
| `:PdftyForward [pdf]` | Show and outline the paragraph at the cursor. |
| `:PdftyMove <page>` | Go to a page (1-based). |
| `:PdftyNext [n]` / `:PdftyPrev [n]` | Move by pages. |
| `:PdftyFirst` / `:PdftyLast` | First / last page. |
| `:PdftyZoom <25–500\|in\|out\|reset>` | Set or step the zoom. |
| `:PdftyView [single\|scroll\|toggle]` | Set the view mode. |
| `:PdftyReload` | Reload the PDF. |
| `:PdftyCenter` | Center the point under the viewer cursor. |
| `:PdftyClear` | Clear selection and outline. |
| `:PdftySwitch [pdf]` | Pick or show an open PDF. |
| `:PdftyNextPdf` / `:PdftyPrevPdf` | Cycle PDFs in the viewer. |

## Code layout

| File | Role |
| --- | --- |
| `src/app/mod.rs` | State, event loop, documents, tile cache and render scheduling |
| `src/app/draw.rs` | Frame composition, row diffing, terminal scroll reuse, status line, panels |
| `src/app/view.rs` | Scrolling, paging, zoom, cursor, tile addressing and overlays |
| `src/app/input.rs` | Keys and mouse |
| `src/app/search.rs` | IPC commands, SyncTeX search, selection, clipboard |
| `src/render.rs` | MuPDF worker pool: tiles, overlays, text extraction |
| `src/layout.rs` | Page placement on a cell canvas (pure, tested) |
| `src/kitty.rs` | Graphics protocol, placeholders, tmux passthrough |
| `src/synctex.rs` | SyncTeX parser with forward and inverse queries |
| `src/nvim.rs` | Neovim msgpack-RPC client and focus handling |
| `src/ipc.rs` | Per-PDF and control Unix sockets |
| `lua/pdfty/init.lua` | Neovim commands, viewer pairing, inverse-search target window |
| `lua/pdfty/terminal.lua` | The side panel: split, viewer job, outer tty detection |

## Tests

```sh
cd sample && latexmk -lualatex -synctex=1 -outdir=build main.tex && cd ..
cargo test
nvim --headless -u NONE -l tests/nvim_commands.lua
cargo build --release && nvim --headless -u NONE -l tests/panel.lua
```

`tests/panel.lua` runs the real viewer in the panel and checks forward and
inverse search, focus, toggling and cleanup.

The Rust tests render the sample with MuPDF, round-trip SyncTeX on it, and
cover the layout, placeholder encoding, IPC and the Neovim RPC client.
