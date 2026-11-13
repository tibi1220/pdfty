-- The pdfty side panel: a terminal split running the viewer, in the spirit of
-- claudecode.nvim. The viewer keeps running while the panel is hidden, so
-- toggling it is instant.
--
-- Neovim's terminal emulator drops Kitty graphics commands but keeps the
-- placeholder text pdfty draws. The viewer therefore sends its images straight
-- to the terminal device Neovim itself is drawn on (PDFTY_TTY); Neovim then
-- redraws the placeholder cells and Kitty fills them with the pages.

local T = {}

local state = { buf = nil, win = nil, job = nil, pid = nil }
local opts
local warned = false

function T.setup(config)
	opts = config
end

local function report(message, level)
	if opts.notifications then
		vim.notify("pdfty: " .. message, level)
	end
end

function T.is_running()
	return state.job ~= nil and vim.fn.jobwait({ state.job }, 0)[1] == -1
end

--- The panel window if it is visible in the current tab.
function T.window()
	if state.win and vim.api.nvim_win_is_valid(state.win) and vim.api.nvim_win_get_buf(state.win) == state.buf then
		if vim.api.nvim_win_get_tabpage(state.win) == vim.api.nvim_get_current_tabpage() then
			return state.win
		end
	end
	return nil
end

function T.is_panel_window(win)
	return state.buf ~= nil and vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == state.buf
end

--- The viewer's control socket (accepts :PdftyAdd requests).
function T.control_socket()
	if not state.pid then
		return nil
	end
	local dir = (vim.env.TMPDIR and vim.env.TMPDIR ~= "" and vim.env.TMPDIR or "/tmp"):gsub("/+$", "")
	return string.format("%s/pdfty-control-%d.sock", dir, state.pid)
end

--- The terminal device Neovim's UI draws on. With Neovim 0.10+ the UI runs in
--- the parent of the server process, so walk up until a process has a tty.
function T.outer_tty()
	if vim.env.PDFTY_TTY and vim.env.PDFTY_TTY ~= "" then
		return vim.env.PDFTY_TTY
	end
	local pid = vim.fn.getpid()
	for _ = 1, 5 do
		local out = vim.fn.system({ "ps", "-o", "tty=,ppid=", "-p", tostring(pid) })
		local tty, ppid = out:match("^%s*(%S+)%s+(%d+)")
		if not tty then
			return nil
		end
		if tty ~= "??" and tty ~= "?" and tty ~= "-" then
			return tty:match("^/dev/") and tty or "/dev/" .. tty
		end
		pid = tonumber(ppid)
		if not pid or pid <= 1 then
			return nil
		end
	end
	return nil
end

local window_options = {
	number = false,
	relativenumber = false,
	signcolumn = "no",
	foldcolumn = "0",
	statuscolumn = "",
	cursorline = false,
	cursorcolumn = false,
	colorcolumn = "",
	spell = false,
	list = false,
	wrap = false,
	sidescrolloff = 0,
	scrolloff = 0,
	winfixwidth = true,
}

local function open_window(enter)
	local previous = vim.api.nvim_get_current_win()
	local width = math.max(20, math.floor(vim.o.columns * opts.terminal.split_width_percentage))
	local position = opts.terminal.split_side == "left" and "topleft" or "botright"
	vim.cmd(string.format("%s vertical %dsplit", position, width))
	local win = vim.api.nvim_get_current_win()
	if state.buf and vim.api.nvim_buf_is_valid(state.buf) then
		vim.api.nvim_win_set_buf(win, state.buf)
	end
	for name, value in pairs(window_options) do
		vim.wo[win][name] = value
	end
	state.win = win
	if not enter and vim.api.nvim_win_is_valid(previous) then
		vim.api.nvim_set_current_win(previous)
	end
	return win
end

local function insert_if_wanted()
	if opts.terminal.auto_insert and T.window() == vim.api.nvim_get_current_win() then
		vim.cmd("startinsert")
	end
end

-- Ctrl-H/J/K/L leave the panel even while its terminal has the keys, like
-- moving between windows anywhere else. vim-tmux-navigator is used when
-- installed, so the keys also cross into neighbouring tmux panes.
local directions = { h = "Left", j = "Down", k = "Up", l = "Right" }

function T.navigate(key)
	local command = "TmuxNavigate" .. directions[key]
	if vim.fn.exists(":" .. command) == 2 then
		vim.cmd(command)
	else
		vim.cmd("wincmd " .. key)
	end
end

local function map_navigation(buf)
	if not opts.terminal.navigation_keys then
		return
	end
	for key, direction in pairs(directions) do
		vim.keymap.set("t", "<C-" .. key .. ">", function()
			T.navigate(key)
		end, { buffer = buf, desc = "pdfty: go to the window " .. direction:lower() })
	end
end

local function on_exit(code)
	local buf = state.buf
	local lines = buf and vim.api.nvim_buf_is_valid(buf) and vim.api.nvim_buf_get_lines(buf, 0, -1, false) or {}
	state.job, state.pid = nil, nil
	-- 129 is SIGHUP from closing the buffer; 0 is a normal quit.
	if code ~= 0 and code ~= 129 then
		local message = {}
		for _, line in ipairs(lines) do
			if line:match("%S") then
				table.insert(message, line)
			end
		end
		report(string.format("viewer exited with code %d: %s", code, table.concat(message, " ")), vim.log.levels.ERROR)
		if not opts.terminal.auto_close then
			return
		end
	end
	if state.win and vim.api.nvim_win_is_valid(state.win) and #vim.api.nvim_list_wins() > 1 then
		pcall(vim.api.nvim_win_close, state.win, true)
	end
	if buf and vim.api.nvim_buf_is_valid(buf) then
		pcall(vim.api.nvim_buf_delete, buf, { force = true })
	end
	state.buf, state.win = nil, nil
end

local function start(pdf, enter)
	if not vim.o.termguicolors and not warned then
		warned = true
		report("'termguicolors' is off; pages need 24-bit colour to appear", vim.log.levels.WARN)
	end
	state.buf = vim.api.nvim_create_buf(false, true)
	local win = open_window(true)
	local cmd = { opts.command }
	if not opts.debug_lines then
		table.insert(cmd, "--no-debug-lines")
	end
	if pdf then
		vim.list_extend(cmd, { "--synctex", pdf })
	end
	local job_opts = {
		-- PDFTY_NVIM_UI: send image commands through nvim_ui_send (0.12+), which
		-- writes them between Neovim's own redraws. PDFTY_TTY is then only
		-- used to measure the cell size; older Neovim also writes to it.
		env = {
			PDFTY_TTY = T.outer_tty(),
			PDFTY_NVIM_UI = vim.fn.exists("*nvim_ui_send") == 1 and "1" or nil,
		},
		on_exit = function(_, code)
			vim.schedule(function()
				on_exit(code)
			end)
		end,
	}
	vim.api.nvim_win_call(win, function()
		if vim.fn.has("nvim-0.11") == 1 then
			job_opts.term = true
			state.job = vim.fn.jobstart(cmd, job_opts)
		else
			state.job = vim.fn.termopen(cmd, job_opts)
		end
	end)
	if not state.job or state.job <= 0 then
		report("could not start " .. opts.command, vim.log.levels.ERROR)
		state.job = nil
		return
	end
	state.pid = vim.fn.jobpid(state.job)
	pcall(vim.api.nvim_buf_set_name, state.buf, "pdfty://panel")
	vim.bo[state.buf].bufhidden = "hide"
	vim.bo[state.buf].buflisted = false
	-- Lets other configs recognise the panel (layouts, minimaps, statuslines).
	vim.bo[state.buf].filetype = "pdfty"
	map_navigation(state.buf)
	vim.api.nvim_create_autocmd("BufEnter", {
		buffer = state.buf,
		callback = function()
			vim.schedule(insert_if_wanted)
		end,
	})
	if enter then
		insert_if_wanted()
	else
		vim.cmd("wincmd p")
	end
end

--- Show the panel, starting the viewer if needed.
--- `pdf` is only used when the viewer is not running yet.
function T.open(pdf, enter)
	if T.is_running() then
		local win = T.window()
		if not win then
			win = open_window(enter)
		elseif enter then
			vim.api.nvim_set_current_win(win)
		end
		if enter then
			insert_if_wanted()
		end
		return
	end
	start(pdf, enter)
end

function T.close()
	local win = T.window()
	if win and #vim.api.nvim_list_wins() > 1 then
		vim.api.nvim_win_close(win, false)
	end
end

function T.toggle(pdf)
	if T.window() then
		T.close()
	else
		T.open(pdf, true)
	end
end

--- Focus when unfocused, hide when focused (like :ClaudeCodeFocus).
function T.focus(pdf)
	local win = T.window()
	if win and win == vim.api.nvim_get_current_win() then
		T.close()
	else
		T.open(pdf, true)
	end
end

--- Quit the viewer politely so it can free its images in Kitty.
function T.stop()
	if T.is_running() then
		vim.fn.chansend(state.job, "q")
		vim.wait(500, function()
			return not T.is_running()
		end, 20)
		if T.is_running() then
			vim.fn.jobstop(state.job)
		end
	end
end

return T
