-- Run with: cargo build --release && nvim --headless -u NONE -l tests/panel.lua
-- Drives the real viewer in the side panel. Graphics go to a capture file.
local root = vim.fn.getcwd()
local gfx = vim.fn.tempname()
io.open(gfx, "w"):close()
vim.opt.runtimepath:prepend(root)
vim.o.termguicolors = true
vim.env.PDFTY_TTY = gfx
-- Headless Neovim has no terminal UI to receive nvim_ui_send, so exercise the
-- direct-tty path, whose writes can be captured in a file.
local exists = vim.fn.exists
vim.fn.exists = function(name)
	return name == "*nvim_ui_send" and 0 or exists(name)
end
local pdfty = require("pdfty").setup({ command = root .. "/target/release/pdfty", auto_register = false })
local term = require("pdfty.terminal")
local function check(cond, msg) if not cond then print("FAIL: " .. msg); vim.cmd("cquit 1") end print("ok: " .. msg) end

vim.cmd.edit(root .. "/sample/main.tex")
local editor = vim.api.nvim_get_current_win()
vim.cmd("Pdfty")
check(term.is_running(), "viewer started in the panel")
local panel = term.window()
check(panel and #vim.api.nvim_tabpage_list_wins(0) == 2, "panel split opened")
check(vim.api.nvim_win_get_position(panel)[2] > 0, "panel is on the right")
check(vim.api.nvim_get_current_win() == panel, ":Pdfty focuses the panel")
local socket = vim.trim(vim.fn.system({ root .. "/target/release/pdfty", "--get-socket", root .. "/sample/build/main.pdf" }))
check(vim.wait(5000, function() return vim.uv.fs_stat(socket) ~= nil end, 50), "viewer opened the current file's PDF (build/main.pdf)")
local function panel_text() return table.concat(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(panel), 0, -1, false), "\n") end
check(vim.wait(5000, function() return panel_text():find("\u{10EEEE}") ~= nil end, 50), "page placeholders reach the terminal buffer")
local function gfx_data() local f = io.open(gfx, "rb"); local d = f:read("*a"); f:close(); return d end
check(gfx_data():find("\27_Ga=T", 1, true) ~= nil, "image uploads go to PDFTY_TTY, not through Neovim")
check(not panel_text():find("_Ga=", 1, true), "no graphics commands leak into the terminal buffer")

-- Forward search from the editor, then inverse search from inside the panel.
vim.api.nvim_set_current_win(editor)
vim.api.nvim_win_set_cursor(editor, { 96, 0 })
vim.cmd("PdftyForward")
check(vim.wait(5000, function() return panel_text():find("2/2", 1, true) ~= nil end, 50), "forward search reached page 2")
vim.wait(300)
vim.api.nvim_win_set_cursor(editor, { 1, 0 })
vim.api.nvim_set_current_win(panel)
vim.fn.chansend(vim.bo[vim.api.nvim_win_get_buf(panel)].channel, "i")
check(vim.wait(3000, function() return vim.api.nvim_win_get_cursor(editor)[1] == 96 end, 50), "inverse search moved the editor window to line 96")
check(term.window() == panel and vim.api.nvim_win_get_buf(panel) ~= vim.api.nvim_win_get_buf(editor), "the panel still shows the viewer")
check(vim.api.nvim_get_current_win() == panel, "plain inverse search keeps focus in the panel")
vim.fn.chansend(vim.bo[vim.api.nvim_win_get_buf(panel)].channel, "I")
check(vim.wait(3000, function() return vim.api.nvim_get_current_win() == editor end, 50), "Shift+I moves focus to the editor window")

-- Ctrl-H/J/K/L are terminal-mode maps in the panel (key delivery in terminal
-- mode can't be simulated under -l; the maps and their action are checked).
vim.api.nvim_set_current_win(term.window())
for _, key in ipairs({ "h", "j", "k", "l" }) do
	check(vim.fn.maparg("<C-" .. key .. ">", "t", false, true).buffer == 1, "Ctrl-" .. key:upper() .. " is mapped in the panel")
end
vim.api.nvim_set_current_win(term.window())
term.navigate("h")
check(vim.api.nvim_get_current_win() == editor, "Ctrl-H's action goes to the editor window on the left")

-- Toggle keeps the viewer alive.
vim.cmd("Pdfty")
check(term.window() == nil and term.is_running(), "toggle hides the panel, viewer keeps running")
vim.cmd("PdftyFocus")
check(term.window() ~= nil and vim.api.nvim_get_current_win() == term.window(), "PdftyFocus shows and focuses it")
vim.cmd("PdftyFocus")
check(term.window() == nil, "PdftyFocus hides it when focused")

term.stop()
check(not term.is_running(), "viewer quits on stop")
check(gfx_data():find("a=d,d=I", 1, true) ~= nil, "viewer freed its Kitty images on exit")
vim.wait(300)
check(term.window() == nil and #vim.api.nvim_list_wins() == 1, "panel cleaned up")
print("PANEL TEST PASSED")
vim.cmd("qa!")
