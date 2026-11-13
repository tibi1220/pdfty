local M = {}
local config
local state = { keymaps = {} }
local terminal = require("pdfty.terminal")

local defaults = {
    command = "pdfty",
    auto_register = true,
    register_interval = 5000,
    register_events = { "BufEnter", "BufWritePost", "FocusLost" },
    notifications = true,
    keymaps = {},
    debug_lines = true,                -- show the last three log lines under the page (toggle with D)
    terminal = {
        split_side = "right",          -- "left" or "right"
        split_width_percentage = 0.40,
        auto_insert = true,            -- enter terminal mode when the panel is focused
        auto_close = true,             -- close the panel when the viewer exits
        auto_open = true,              -- :PdftyAdd / :PdftyForward open the panel when no viewer runs
        navigation_keys = true,        -- Ctrl-H/J/K/L in the panel move to the neighbouring window
    },
}

local function report(message, level)
    if config.notifications then vim.notify(message, level) end
end

local function canonical(path)
	local absolute = vim.fn.fnamemodify(path, ":p")
	return vim.uv.fs_realpath(absolute) or absolute
end

local function viewer_command()
    return config.command
end

local function active_viewers()
	local command = viewer_command()
	if not command then
		return nil, "pdfty binary not found"
	end
	local lines = vim.fn.systemlist({ command, "--list" })
	if vim.v.shell_error ~= 0 then
		return nil, "pdfty --list failed"
	end
	local viewers = {}
	for _, line in ipairs(lines) do
		local socket, pdf = line:match("^(.-)\t(.+)$")
		if socket and pdf then
			table.insert(viewers, { socket = socket, pdf = canonical(pdf) })
		elseif line:match("^.+%.sock$") then
			table.insert(viewers, { socket = line })
		end
	end
	return viewers
end

local function vimtex_pdf()
    if config.resolve_pdf then
        local pdf = config.resolve_pdf(vim.api.nvim_get_current_buf())
        if type(pdf) == "string" and pdf ~= "" then return canonical(pdf) end
    end
	local ok, pdf = pcall(vim.api.nvim_eval, "b:vimtex.compiler.get_file('pdf')")
	if ok and type(pdf) == "string" and pdf ~= "" and vim.fn.filereadable(pdf) == 1 then
		return canonical(pdf)
	end
end

local function source_for_buffer()
	local source = vim.api.nvim_buf_get_name(0)
	if source == "" or vim.fn.fnamemodify(source, ":e") ~= "tex" then
		return nil
	end
	return canonical(source)
end

-- The compiled PDF for the current buffer: explicit path, resolve_pdf/VimTeX,
-- or a PDF beside the source (or in its build/ directory).
local function resolve_pdf(requested)
	if requested and requested ~= "" then
		local pdf = canonical(requested)
		return vim.fn.filereadable(pdf) == 1 and pdf or nil
	end
	local pdf = vimtex_pdf()
	if pdf then
		return pdf
	end
	local source = source_for_buffer()
	if not source then
		return nil
	end
	local dir, name = vim.fn.fnamemodify(source, ":h"), vim.fn.fnamemodify(source, ":t:r")
	for _, candidate in ipairs({ dir .. "/" .. name .. ".pdf", dir .. "/build/" .. name .. ".pdf" }) do
		if vim.fn.filereadable(candidate) == 1 then
			return canonical(candidate)
		end
	end
end

-- Whether the viewer's PDF was built from this source (reads its SyncTeX file).
local function builds_from(pdf, source)
	vim.fn.systemlist({ viewer_command(), "--has-source", pdf, source })
	return vim.v.shell_error == 0
end

local function find_viewer(source, requested_pdf)
	local viewers, err = active_viewers()
	if not viewers then
		return nil, err
	end
	local has_pdf = false
	for _, viewer in ipairs(viewers) do
		if viewer.pdf then
			has_pdf = true
		end
	end
	if not has_pdf then
		return nil, "no PDF added; run :PdftyAdd"
	end
	local preferred = requested_pdf and canonical(requested_pdf) or vimtex_pdf()
	if preferred then
		for _, viewer in ipairs(viewers) do
			if viewer.pdf == preferred then
				return viewer
			end
		end
		if requested_pdf then
			return nil, "no active viewer for " .. preferred
		end
	end
	for _, viewer in ipairs(viewers) do
		if source and viewer.pdf and builds_from(viewer.pdf, source) then
			return viewer
		end
	end
	if not source then
		local only
		for _, viewer in ipairs(viewers) do
			if viewer.pdf then
				if only then
					return nil, "open a paired TeX buffer or use :PdftySwitch"
				end
				only = viewer
			end
		end
		return only
	end
	return nil, "no active viewer matches this TeX source"
end

local function send_to(socket, message, notify)
	local client = vim.uv.new_pipe(false)
	client:connect(socket, function(connect_err)
		if connect_err then
			if notify then
				vim.schedule(function()
					report("pdfty: " .. connect_err, vim.log.levels.ERROR)
				end)
			end
			client:close()
			return
		end
		client:write(vim.json.encode(message) .. "\n")
		client:read_start(function(read_err, data)
			if notify then
				if read_err or (data and data ~= "ok\n") then
					vim.schedule(function()
						report("pdfty: " .. (read_err or data), vim.log.levels.ERROR)
					end)
				elseif data == "ok\n" and message.action == "add" then
					vim.schedule(function()
						report("pdfty: added " .. message.pdf)
					end)
				elseif data == "ok\n" and message.action == "register" then
					vim.schedule(function()
						report("pdfty: connected to " .. message.pdf)
					end)
				end
			end
			client:close()
		end)
	end)
end

local function server_name()
	local server = vim.v.servername
	if not server or server == "" then
		server = vim.fn.serverstart()
	end
	return server
end

-- Make sure the side panel shows `pdf`, then call back with its socket once
-- the viewer has opened it.
local function with_panel_viewer(pdf, callback)
	if terminal.is_running() then
		terminal.open(nil, false)
		send_to(terminal.control_socket(), { action = "add", pdf = pdf, nvim = server_name() }, false)
	else
		terminal.open(pdf, false)
	end
	local socket = vim.trim(vim.fn.system({ config.command, "--get-socket", pdf }))
	local tries = 0
	local function check()
		if vim.uv.fs_stat(socket) then
			callback({ socket = socket, pdf = pdf })
		elseif tries < 50 and terminal.is_running() then
			tries = tries + 1
			vim.defer_fn(check, 100)
		else
			report("pdfty: the viewer did not open " .. pdf, vim.log.levels.ERROR)
		end
	end
	check()
end

local function send(action, requested_pdf, notify)
	local source = source_for_buffer()
	if not source then
		if notify then
			report("pdfty: open a TeX source file first", vim.log.levels.ERROR)
		end
		return
	end
	local pos = vim.api.nvim_win_get_cursor(0)
	local function deliver(viewer)
		local message = { action = action, pdf = viewer.pdf, nvim = server_name() }
		if action == "forward" then
			message.source, message.line, message.column = source, pos[1], pos[2] + 1
		end
		send_to(viewer.socket, message, notify)
	end
	local viewer, err = find_viewer(source, requested_pdf)
	if viewer then
		return deliver(viewer)
	end
	local pdf = action == "forward" and config.terminal.auto_open and resolve_pdf(requested_pdf)
	if pdf then
		return with_panel_viewer(pdf, deliver)
	end
	if notify then
		report("pdfty: " .. err, vim.log.levels.ERROR)
	end
end

local function register_commands()
	vim.api.nvim_create_user_command("PdftyAdd", function(opts)
		local source = source_for_buffer()
		if not source and opts.args == "" then
			report("pdfty: open a TeX source file first", vim.log.levels.ERROR)
			return
		end
		local pdf = resolve_pdf(opts.args)
		if not pdf then
			report("pdfty: compiled PDF not found; pass its path to :PdftyAdd", vim.log.levels.ERROR)
			return
		end
		if terminal.is_running() then
			terminal.open(nil, false)
			send_to(terminal.control_socket(), { action = "add", pdf = pdf, nvim = server_name() }, true)
			return
		end
		local viewers, err = active_viewers()
		if not viewers then
			report("pdfty: " .. err, vim.log.levels.ERROR)
			return
		end
		for _, viewer in ipairs(viewers) do
			if not viewer.pdf then
				send_to(viewer.socket, { action = "add", pdf = pdf, nvim = server_name() }, true)
				return
			end
		end
		if config.terminal.auto_open then
			terminal.open(pdf, false)
			return
		end
		report("pdfty: no control socket; start pdfty without arguments or with --synctex", vim.log.levels.ERROR)
	end, { nargs = "?", complete = "file" })

	-- Side panel, like claudecode.nvim's :ClaudeCode / :ClaudeCodeFocus.
	local function panel_pdf(args)
		return resolve_pdf(args)
	end
	vim.api.nvim_create_user_command("Pdfty", function(opts)
		terminal.toggle(panel_pdf(opts.args))
	end, { nargs = "?", complete = "file", desc = "Toggle the pdfty side panel" })
	vim.api.nvim_create_user_command("PdftyFocus", function(opts)
		terminal.focus(panel_pdf(opts.args))
	end, { nargs = "?", complete = "file", desc = "Focus the pdfty panel, or hide it when focused" })
	vim.api.nvim_create_user_command("PdftyOpen", function(opts)
		local pdf = panel_pdf(opts.args)
		if terminal.is_running() and pdf then
			send_to(terminal.control_socket(), { action = "add", pdf = pdf, nvim = server_name() }, true)
		end
		terminal.open(pdf, true)
	end, { nargs = "?", complete = "file", desc = "Open the pdfty panel (and the given or current PDF)" })
	vim.api.nvim_create_user_command("PdftyClose", function()
		terminal.close()
	end, { desc = "Hide the pdfty panel; the viewer keeps running" })

	vim.api.nvim_create_user_command("PdftyRegister", function(opts)
		send("register", opts.args ~= "" and opts.args or nil, true)
	end, { nargs = "?", complete = "file", desc = "Pair the current TeX buffer with pdfty" })

	vim.api.nvim_create_user_command("PdftyForward", function(opts)
		send("forward", opts.args ~= "" and opts.args or nil, true)
	end, { nargs = "?", complete = "file", desc = "Show the PDF paragraph at the source cursor" })

	local function control(command, fields, requested_pdf)
		local viewer, err = find_viewer(source_for_buffer(), requested_pdf)
		if not viewer then
			report("pdfty: " .. err, vim.log.levels.ERROR)
			return
		end
		local message = vim.tbl_extend("force", {
			action = "control",
			command = command,
			pdf = viewer.pdf,
			nvim = server_name(),
		}, fields or {})
		send_to(viewer.socket, message, true)
	end

	local function integer(value, minimum, maximum, label)
		local number = value:match("^%d+$") and tonumber(value)
		if not number or number < minimum or number > maximum then
			report(
				string.format("pdfty: %s must be an integer between %d and %d", label, minimum, maximum),
				vim.log.levels.ERROR
			)
			return
		end
		return number
	end

	vim.api.nvim_create_user_command("PdftyMove", function(opts)
		local page = integer(opts.args, 1, 4294967295, "page")
		if page then
			control("move", { page = page })
		end
	end, { nargs = 1, desc = "Jump to PDF page (numbered from 1)" })

	for name, command in pairs({ PdftyNext = "next", PdftyPrev = "prev" }) do
		vim.api.nvim_create_user_command(name, function(opts)
			local count = integer(opts.args ~= "" and opts.args or "1", 1, 4294967295, "page count")
			if count then
				control(command, { count = count })
			end
		end, { nargs = "?", desc = "Move through PDF pages; optional count defaults to 1" })
	end

	for name, command in pairs({
		PdftyFirst = "first",
		PdftyLast = "last",
		PdftyReload = "reload",
		PdftyCenter = "center",
		PdftyClear = "clear",
		PdftyNextPdf = "next_pdf",
		PdftyPrevPdf = "prev_pdf",
	}) do
		vim.api.nvim_create_user_command(name, function()
			control(command)
		end, {
			desc = ({
				first = "Jump to the first PDF page",
				last = "Jump to the last PDF page",
				reload = "Reload the PDF",
				center = "Center the document point under the cursor",
				clear = "Clear text selection and search highlights",
				next_pdf = "Show the next PDF in the viewer",
				prev_pdf = "Show the previous PDF in the viewer",
			})[command],
		})
	end

	local function completion(values)
		return function(prefix)
			return vim.tbl_filter(function(value)
				return value:sub(1, #prefix) == prefix
			end, values)
		end
	end

	vim.api.nvim_create_user_command("PdftyZoom", function(opts)
		local commands = { ["in"] = "zoom_in", out = "zoom_out", reset = "zoom_reset" }
		if commands[opts.args] then
			control(commands[opts.args])
		else
			local percent = integer(opts.args, 25, 500, "zoom percent")
			if percent then
				control("zoom", { percent = percent })
			end
		end
	end, {
		nargs = 1,
		complete = completion({ "in", "out", "reset", "50", "75", "100", "125", "150", "200", "300", "400" }),
		desc = "Set zoom (25–500), zoom in/out by 25%, or reset zoom",
	})

	vim.api.nvim_create_user_command("PdftyView", function(opts)
		local mode = opts.args ~= "" and opts.args or "toggle"
		if mode ~= "single" and mode ~= "scroll" and mode ~= "toggle" then
			report("pdfty: view must be single, scroll, or toggle", vim.log.levels.ERROR)
			return
		end
		control("view", { mode = mode })
	end, { nargs = "?", complete = completion({ "single", "scroll", "toggle" }), desc = "Set or toggle the PDF view mode" })

	vim.api.nvim_create_user_command("PdftySwitch", function(opts)
		if opts.args ~= "" then
			control("switch", nil, opts.args)
			return
		end
		local viewers, err = active_viewers()
		if not viewers then
			report("pdfty: " .. err, vim.log.levels.ERROR)
			return
		end
		local documents = vim.tbl_filter(function(viewer)
			return viewer.pdf ~= nil
		end, viewers)
		if #documents == 0 then
			report("pdfty: no PDF added; run :PdftyAdd", vim.log.levels.ERROR)
			return
		end
		vim.ui.select(documents, {
			prompt = "Show PDF:",
			format_item = function(viewer)
				return viewer.pdf
			end,
		}, function(viewer)
			if viewer then
				send_to(
					viewer.socket,
					{ action = "control", command = "switch", pdf = viewer.pdf, nvim = server_name() },
					true
				)
			end
		end)
	end, { nargs = "?", complete = "file", desc = "Select an open PDF, or show the given PDF path" })

end

local function auto_register()
	if config.auto_register and source_for_buffer() then
		send("register", nil, false)
	end
end

local mapping_commands = {
    toggle = "Pdfty", focus = "PdftyFocus",
    add = "PdftyAdd", register = "PdftyRegister", forward = "PdftyForward",
    move = "PdftyMove", next = "PdftyNext", prev = "PdftyPrev",
    first = "PdftyFirst", last = "PdftyLast", switch = "PdftySwitch",
    next_pdf = "PdftyNextPdf", prev_pdf = "PdftyPrevPdf",
    zoom_in = "PdftyZoom in", zoom_out = "PdftyZoom out", zoom_reset = "PdftyZoom reset",
    view = "PdftyView", reload = "PdftyReload",
    center = "PdftyCenter", clear = "PdftyClear",
}

local function stop_timer()
    if state.timer then
        state.timer:stop()
        if not state.timer:is_closing() then state.timer:close() end
        state.timer = nil
    end
end

--- Called by the viewer for inverse search (over Neovim's RPC socket).
--- Shows `file` at `line` in an editor window, never in the panel itself.
--- Returns true when the panel is in this Neovim, i.e. focus is handled here.
function M.open_source(file, line, focus)
	local panel = terminal.window()
	local buf = vim.fn.bufadd(file)
	local function usable(win)
		if not vim.api.nvim_win_is_valid(win) or terminal.is_panel_window(win) then
			return false
		end
		if vim.api.nvim_win_get_config(win).relative ~= "" then
			return false
		end
		return vim.bo[vim.api.nvim_win_get_buf(win)].buftype == ""
	end
	local windows = vim.tbl_filter(usable, vim.api.nvim_tabpage_list_wins(0))
	local target
	for _, win in ipairs(windows) do
		if vim.api.nvim_win_get_buf(win) == buf then
			target = win
		end
	end
	if not target and state.editor_win and vim.tbl_contains(windows, state.editor_win) then
		target = state.editor_win
	end
	target = target or windows[1]
	if not target then
		local side = config.terminal.split_side == "left" and "botright" or "topleft"
		vim.cmd(side .. " vsplit")
		target = vim.api.nvim_get_current_win()
	end
	vim.api.nvim_win_call(target, function()
		vim.cmd("normal! m'")
		if vim.api.nvim_win_get_buf(target) ~= buf then
			vim.cmd("keepalt buffer " .. buf)
		end
		line = math.max(1, math.min(line, vim.api.nvim_buf_line_count(buf)))
		vim.api.nvim_win_set_cursor(target, { line, 0 })
		vim.cmd("normal! zvzz")
	end)
	if focus then
		vim.api.nvim_set_current_win(target)
		vim.cmd("stopinsert")
	end
	return panel ~= nil
end

function M.setup(opts)
    opts = opts or {}
    local next_config = vim.tbl_deep_extend("force", vim.deepcopy(defaults), opts)
    if opts.command == nil and vim.g.pdfty_command then next_config.command = vim.g.pdfty_command end
    assert(type(next_config.command) == "string" and next_config.command ~= "", "pdfty: command must be an executable name or path")
    assert(type(next_config.auto_register) == "boolean", "pdfty: auto_register must be boolean")
    assert(type(next_config.notifications) == "boolean", "pdfty: notifications must be boolean")
    assert(type(next_config.register_interval) == "number" and next_config.register_interval >= 0
        and next_config.register_interval % 1 == 0, "pdfty: register_interval must be a nonnegative integer")
    assert(type(next_config.register_events) == "table", "pdfty: register_events must be a list")
    assert(next_config.resolve_pdf == nil or type(next_config.resolve_pdf) == "function", "pdfty: resolve_pdf must be a function")
    assert(type(next_config.keymaps) == "table", "pdfty: keymaps must be a table")
    for action, lhs in pairs(next_config.keymaps) do
        assert(mapping_commands[action], "pdfty: unknown keymap action " .. tostring(action))
        assert(lhs == false or (type(lhs) == "string" and lhs ~= ""), "pdfty: keymap must be a nonempty string or false")
    end
    assert(type(next_config.debug_lines) == "boolean", "pdfty: debug_lines must be boolean")
    assert(type(next_config.terminal) == "table", "pdfty: terminal must be a table")
    assert(next_config.terminal.split_side == "left" or next_config.terminal.split_side == "right",
        "pdfty: terminal.split_side must be \"left\" or \"right\"")
    local pct = next_config.terminal.split_width_percentage
    assert(type(pct) == "number" and pct > 0 and pct < 1, "pdfty: terminal.split_width_percentage must be between 0 and 1")
    stop_timer()
    config = next_config
    terminal.setup(config)
    M.config = config
    M.configured = true
    local group = vim.api.nvim_create_augroup("Pdfty", { clear = true })
    for _, name in ipairs({ "PdfViewAdd", "PdfViewRegister", "PdfViewForward" }) do
        pcall(vim.api.nvim_del_user_command, name)
    end
    register_commands()
    for _, lhs in ipairs(state.keymaps) do pcall(vim.keymap.del, "n", lhs) end
    state.keymaps = {}
    for action, lhs in pairs(config.keymaps) do
        if lhs then
            local command = mapping_commands[action]
            vim.keymap.set("n", lhs, function()
                if action == "move" or action == "next" or action == "prev" then
                    vim.cmd(command .. " " .. vim.v.count1)
                else
                    vim.cmd(command)
                end
            end, { desc = command, silent = true })
            table.insert(state.keymaps, lhs)
        end
    end
    -- Remember the last editor window so inverse search lands there.
    vim.api.nvim_create_autocmd("WinLeave", {
        group = group,
        callback = function()
            local win = vim.api.nvim_get_current_win()
            if not terminal.is_panel_window(win) and vim.bo.buftype == ""
                and vim.api.nvim_win_get_config(win).relative == "" then
                state.editor_win = win
            end
        end,
    })
    vim.api.nvim_create_autocmd("VimLeavePre", { group = group, callback = terminal.stop })
    if config.auto_register then
        if #config.register_events > 0 then
            vim.api.nvim_create_autocmd(config.register_events, {
                group = group, pattern = "*.tex",
                callback = function() vim.schedule(auto_register) end,
            })
        end
        if config.register_interval > 0 then
            state.timer = vim.uv.new_timer()
            state.timer:start(math.min(1000, config.register_interval), config.register_interval,
                function() vim.schedule(auto_register) end)
            vim.api.nvim_create_autocmd("VimLeavePre", {
                group = group, once = true, callback = stop_timer,
            })
        end
    end
    return M
end

return M
