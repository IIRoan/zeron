--!strict
-- Each chat's panel sizes, as the Linux fork's desktop app kept them
-- (`chatPanelSizes`): its files panel's width and its shell dock's height.
-- Switching to a chat puts back what it had; a chat never resized keeps the
-- sizes showing. The chat list's width stays shared. At most KEEP chats are
-- kept, the longest unresized dropped first.
local Prefs = require("./prefs")
local route = require("@app/screens/shell/route")
local fork_layout = require("./fork_layout")

local KEEP = 300
local sizes = Prefs.persist("chat_panel_sizes", {} :: any)

local M = {}

-- `table` without its oldest entries past KEEP.
function M.pruned(all: { [string]: any }): { [string]: any }
	local ids = {}
	for id in all do
		table.insert(ids, id)
	end
	if #ids <= KEEP then
		return all
	end
	table.sort(ids, function(a: string, b: string): boolean
		return (all[a].at or 0) > (all[b].at or 0)
	end)
	local out = {}
	for i = 1, KEEP do
		out[ids[i]] = all[ids[i]]
	end
	return out
end

-- Follow the session showing: put its sizes back as it shows, and keep
-- them as they're changed. `files_w` is the page's files panel width.
function M.follow(files_w: any)
	local current: string? = nil
	ui.effect(function()
		local session = route.session:get()
		local id = session and session.id
		if id == current then
			return
		end
		current = id
		local saved = if id then sizes:peek()[id] else nil
		if saved then
			if saved.files then
				files_w:set(saved.files)
			end
			if saved.terminal then
				fork_layout.height:set(saved.terminal)
			end
		end
	end)
	-- Settled sizes (a drag ends): this chat's from now on.
	local files = ui.debounced(files_w, 300)
	local height = ui.debounced(fork_layout.height, 300)
	ui.effect(function()
		local f, h = files:get(), height:get()
		local id = current
		if not id then
			return
		end
		local all = sizes:peek()
		local mine = all[id]
		if mine and mine.files == f and mine.terminal == h then
			return
		end
		local next = table.clone(all)
		next[id] = { files = f, terminal = h, at = os.time() }
		sizes:set(M.pruned(next))
	end)
end

return M
