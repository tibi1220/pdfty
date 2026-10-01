-- Run with: nvim --headless -u NONE -l tests/nvim_commands.lua
-- Exercise real command registration and argument handling with a fake socket.
local root = vim.fn.getcwd()
vim.opt.runtimepath:prepend(root)
local viewer_commands = {}
local pdf = root .. "/sample/build/main.pdf"
local other = root .. "/sample/build/other.pdf"
local messages, notifications = {}, {}
vim.g.pdfty_command = "pdfty-test"
vim.notify = function(message) table.insert(notifications, message) end
vim.fn.systemlist = function(args)
	if args[2] == "--list" then
        table.insert(viewer_commands, args[1])
		return { "/tmp/main.sock\t" .. pdf, "/tmp/other.sock\t" .. other, "/tmp/control.sock" }
	end
	return { "SyncTeX result begin" }
end
vim.uv.new_pipe = function()
	return {
		connect = function(_, _, callback) callback(nil) end,
		write = function(_, data) table.insert(messages, vim.json.decode(data)) end,
		read_start = function(_, callback) callback(nil, "ok\n") end,
		close = function() end,
	}
end
vim.cmd.edit(root .. "/sample/main.tex")
for _, name in ipairs({ "PdfViewAdd", "PdfViewRegister", "PdfViewForward" }) do
	vim.api.nvim_create_user_command(name, function() end, {})
end
dofile(root .. "/plugin/pdfty.lua")
for _, name in ipairs({ "PdfViewAdd", "PdfViewRegister", "PdfViewForward" }) do
	assert(vim.fn.exists(":" .. name) == 0, name .. " was not removed")
end

local function expect(ex, action, command, field, value)
	local count = #messages
	vim.cmd(ex)
	assert(#messages == count + 1, ex .. " did not send exactly one message")
	local message = messages[#messages]
	assert(message.action == action, ex .. " sent wrong action")
	assert(message.command == command, ex .. " sent wrong command")
	assert(message.pdf == pdf or command == "switch", ex .. " targeted wrong PDF")
	if field then assert(message[field] == value, ex .. " sent wrong " .. field) end
end

expect("PdftyMove 2", "control", "move", "page", 2)
expect("PdftyNext", "control", "next", "count", 1)
expect("PdftyNext 3", "control", "next", "count", 3)
expect("PdftyPrev", "control", "prev", "count", 1)
expect("PdftyPrev 4", "control", "prev", "count", 4)
for ex, command in pairs({ PdftyFirst = "first", PdftyLast = "last", PdftyReload = "reload", PdftyCenter = "center", PdftyClear = "clear",
	PdftyNextPdf = "next_pdf", PdftyPrevPdf = "prev_pdf" }) do
	expect(ex, "control", command)
end
expect("PdftyZoom 150", "control", "zoom", "percent", 150)
expect("PdftyZoom in", "control", "zoom_in")
expect("PdftyZoom out", "control", "zoom_out")
expect("PdftyZoom reset", "control", "zoom_reset")
expect("PdftyView", "control", "view", "mode", "toggle")
expect("PdftyView single", "control", "view", "mode", "single")
expect("PdftyView scroll", "control", "view", "mode", "scroll")
expect("PdftyRegister", "register", nil)
expect("PdftyForward", "forward", nil, "line", 1)
expect("PdftyAdd " .. pdf, "add", nil)
expect("PdftySwitch " .. other, "control", "switch", "pdf", other)
vim.ui.select = function(items, _, callback) callback(items[2]) end
expect("PdftySwitch", "control", "switch", "pdf", other)

for _, ex in ipairs({ "PdftyMove 0", "PdftyMove -1", "PdftyMove 1.5", "PdftyMove nonsense",
	"PdftyMove 4294967296", "PdftyNext 0", "PdftyPrev -2", "PdftyZoom 49", "PdftyZoom 301",
	"PdftyView invalid" }) do
	local count, notices = #messages, #notifications
	vim.cmd(ex)
	assert(#messages == count, ex .. " sent an invalid request")
	assert(#notifications == notices + 1, ex .. " failed without reporting the error")
end
assert(not pcall(vim.cmd, "PdftyMove"), "PdftyMove must require a page")
assert(vim.deep_equal(vim.fn.getcompletion("PdftyView s", "cmdline"), { "single", "scroll" }))

-- Loading the entry point twice must retain commands without duplicating hooks.
dofile(root .. "/plugin/pdfty.lua")
assert(#vim.api.nvim_get_autocmds({ group = "Pdfty" }) == 4)
expect("PdftyMove 1", "control", "move", "page", 1)
local pdfty = require("pdfty")
pdfty.setup({ command = "custom-pdfty", auto_register = false, notifications = false,
    keymaps = { next = "<leader>pj", move = "<leader>pm", forward = false },
    resolve_pdf = function(buf) assert(buf == vim.api.nvim_get_current_buf()); return pdf end,
})
assert(#vim.api.nvim_get_autocmds({ group = "Pdfty" }) == 0)
expect("PdftyNext 10", "control", "next", "count", 10)
assert(viewer_commands[#viewer_commands] == "custom-pdfty")
local map = vim.fn.maparg("<leader>pj", "n", false, true)
assert(type(map.callback) == "function")
map.callback()
assert(messages[#messages].command == "next" and messages[#messages].count == 1)
vim.api.nvim_feedkeys("3\\pj", "xt", false)
assert(messages[#messages].command == "next" and messages[#messages].count == 3,
    "numeric count was not passed through the Neovim mapping")
local count = #notifications
vim.cmd("PdftyMove 0")
assert(#notifications == count, "notifications setting ignored")
pdfty.setup({ register_interval = 0, register_events = { "BufWritePost" } })
assert(#vim.api.nvim_get_autocmds({ group = "Pdfty" }) == 1)
assert(vim.fn.maparg("<leader>pj", "n") == "", "old mapping was not removed")
pdfty.setup({ register_events = {}, register_interval = 0 })
assert(#vim.api.nvim_get_autocmds({ group = "Pdfty" }) == 0)
pdfty.setup()
assert(#vim.api.nvim_get_autocmds({ group = "Pdfty" }) == 4)
expect("PdftyForward", "forward", nil, "line", 1)
assert(viewer_commands[#viewer_commands] == "pdfty-test")
print("Neovim commands passed (routing, validation, completion, reload, configuration, mappings)")
vim.cmd.qa()
