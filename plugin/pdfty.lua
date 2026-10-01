-- Register defaults for users who load pdfty directly from runtimepath.
-- Lazy's opts/config can call setup again to apply user settings.
local previous = package.loaded["pdfty.plugin"]
if previous and previous.timer then
    previous.timer:stop()
    if not previous.timer:is_closing() then previous.timer:close() end
end
package.loaded["pdfty.plugin"] = nil
local pdfty = require("pdfty")
if not pdfty.configured then pdfty.setup(vim.g.pdfty_config or {}) end
