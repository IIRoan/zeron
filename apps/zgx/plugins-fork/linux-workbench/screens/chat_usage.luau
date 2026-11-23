--!strict
-- A chat's usage card (src/chat_usage.rs): its own usage, then what it
-- shares with other chats, read again every second while it shows.
--
--   ChatUsage { id, title = fn, project = fn, device = fn, here = fn }

local ds = require("@ds")
local sessions = require("@account/sessions")
local panels = require("@app/screens/panel/state")

local W = 320
-- Milliseconds: the first reading's window, later ones', and the rest between.
local FIRST, WINDOW, REST = 250, 500, 500
local GROUPS, SHARED, TOP = 6, 4, 4

type Amount = { cpu: number, memory: number, read: number, written: number, processes: number }
type Group = Amount & { name: string, shared: boolean }
type Top = Amount & { name: string, count: number }
type Usage = Amount & { threads: number, cores: number, groups: { Group }, top: { Top } }

local NONE: Usage = { cpu = 0, memory = 0, read = 0, written = 0, processes = 0, threads = 0, cores = 0, groups = {}, top = {} }

-- The chat and every chat under it (subagents, side chats), theirs too.
local function family(id: string): { string }
	local out, seen = { id }, { [id] = true }
	local i = 1
	while out[i] do
		for _, child in sessions.children(out[i]) do
			if not seen[child.id] then
				seen[child.id] = true
				table.insert(out, child.id)
			end
		end
		i += 1
	end
	return out
end

local function terminals(ids: { string }): { string }
	local out = {}
	for _, id in ids do
		for _, key in { `chat:{id}`, `dock:{id}` } do
			for _, tab in panels.of(key).tabs do
				if tab.kind == "terminal" and tab.terminal then
					table.insert(out, tab.terminal)
				end
			end
		end
	end
	return out
end

-- Whether a chat outside `ids` works in the same checkout: its services are
-- then that chat's too.
local function checkout_shared(ids: { string }): boolean
	local now = sessions.now:peek()
	local family = {}
	for _, id in ids do
		family[id] = true
	end
	local function folder(c: any): string?
		if c.cwd then
			return c.cwd
		end
		for _, p in now.projects do
			if p.id == c.project then
				return p.path
			end
		end
		return nil
	end
	local me: any = nil
	for _, c in now.chats do
		if c.id == ids[1] then
			me = c
		end
	end
	if not me or not me.project then
		return false
	end
	local mine = folder(me)
	for _, c in now.chats do
		if not family[c.id] and c.project == me.project and c.device == me.device and folder(c) == mine then
			return true
		end
	end
	return false
end

-- 355923968 -> "355.9 MB" (decimal, as the desktop's own monitors count).
local function bytes(n: number): string
	if n < 1000 then
		return `{math.floor(n)} B`
	end
	for _, unit in { "kB", "MB", "GB", "TB" } do
		n /= 1000
		if n < 1000 or unit == "TB" then
			return string.format(if n < 100 then "%.2f %s" else "%.1f %s", n, unit)
		end
	end
	return ""
end

local function percent(n: number): string
	return string.format(if n < 10 then "%.1f%%" else "%.0f%%", n)
end

local function text(value: () -> string, size: string, color: string, extra: { [string]: any }?): Node
	local props: { [string]: any } = { value, literal = true, text_size = size, color = color, tabular = true }
	for k, v in extra or {} do
		props[k] = v
	end
	return ui.text(props)
end

local function stat(name: string, value: () -> string, hint: () -> string): Node
	return ui.row {
		w_full = true,
		gap = "$space.2",
		items = "baseline",
		text(function(): string
			return name
		end, "$font.size.xs", "$text.tertiary", { w = 68, shrink = 0 }),
		text(value, "$font.size.sm", "$text.primary", { weight = "$font.weight.medium", shrink = 0 }),
		text(hint, "$font.size.xs", "$text.tertiary", { grow = 1, min_w = 0, truncate = true, align = "right" }),
	}
end

-- Fixed slots for a list that changes every reading (`at(i)` nil: hidden).
local function slots(n: number, at: (number) -> Amount?, name: (number) -> string, strong: boolean): { Node }
	local rows = {}
	for i = 1, n do
		local function a(): Amount
			return at(i) or NONE
		end
		rows[i] = ui.show(function(): boolean
			return at(i) ~= nil
		end, ui.row {
			w_full = true,
			gap = "$space.2",
			items = "baseline",
			text(function(): string
				return name(i)
			end, "$font.size.xs", if strong then "$text.primary" else "$text.secondary", { grow = 1, min_w = 0, truncate = true }),
			text(function(): string
				return bytes(a().memory)
			end, "$font.size.xs", "$text.secondary", { w = 72, shrink = 0, align = "right" }),
			text(function(): string
				return percent(a().cpu)
			end, "$font.size.xs", "$text.tertiary", { w = 44, shrink = 0, align = "right" }),
		})
	end
	return rows
end

return function(props: { id: string, title: () -> string, project: () -> string?, device: () -> string?, here: () -> boolean }): Node
	local usage = ui.use_state(nil :: Usage?)
	local failed = ui.use_state(false)
	local gone = false
	local cancel: (() -> ())? = nil
	local function read()
		if gone or not props.here() then
			return
		end
		local ids = family(props.id)
		local window = if usage:peek() == nil then FIRST else WINDOW
		-- (An app without the action throws here rather than failing the call.)
		local ok, call = pcall(ui.call, "chat.usage", {
			ids = ids,
			terminals = terminals(ids),
			project = props.project() or ui.null,
			checkout_shared = checkout_shared(ids),
			window = window,
		})
		if not ok then
			failed:set(true)
			return
		end
		call:after(function(u: any)
			if gone then
				return
			end
			usage:set(u)
			cancel = ui.after(REST, read)
		end, function()
			if not gone then
				failed:set(true)
			end
		end)
	end
	read()
	ui.on_cleanup(function()
		gone = true
		if cancel then
			cancel()
		end
	end)

	local function now(): Usage
		return usage:get() or NONE
	end
	local function measured(): boolean
		return usage:get() ~= nil
	end
	local function shown(f: (Usage) -> string): () -> string
		return function(): string
			return if measured() then f(now()) else "—"
		end
	end
	local function groups(shared: boolean): { Group }
		local out = {}
		for _, g in now().groups do
			if g.shared == shared then
				table.insert(out, g)
			end
		end
		return out
	end
	local function idle(): boolean
		return measured() and now().processes == 0
	end
	local function note(): string
		if not props.here() then
			return `Runs on {props.device() or "another device"}: usage shows on the device it runs on.`
		elseif failed:get() then
			return "Usage isn't available in this build."
		elseif idle() then
			return if #groups(true) > 0 then "Nothing of its own is running right now." else "Nothing running for this chat right now."
		end
		return ""
	end
	local function live(): boolean
		return props.here() and not failed:get()
	end

	return ds.Glass {
		kind = "menu",
		class = "chat-usage-card",
		w = W,
		p = "$space.3",
		gap = "$space.2",
		pointer_events = "none",
		text(props.title, "$font.size.md", "$text.primary", { w_full = true, wrap = true, lines = 2, weight = "$font.weight.medium", tabular = false }),
		ui.show(function(): boolean
			return note() ~= ""
		end, text(note, "$font.size.xs", "$text.secondary", { w_full = true, wrap = true })),
		ui.show(function(): boolean
			return live() and not idle()
		end, ui.column {
			w_full = true,
			gap = "$space.2",
			ui.column {
				w_full = true,
				gap = "$space.1",
				stat("CPU", shown(function(u): string
					return percent(u.cpu)
				end), shown(function(u): string
					return string.format("%.2f of %d cores", u.cpu / 100, u.cores)
				end)),
				stat("Memory", shown(function(u): string
					return bytes(u.memory)
				end), function(): string
					return ""
				end),
				stat("Disk", shown(function(u): string
					return `{bytes(u.read)}/s read`
				end), shown(function(u): string
					return `{bytes(u.written)}/s written`
				end)),
				stat("Processes", shown(function(u): string
					return tostring(u.processes)
				end), shown(function(u): string
					return `{u.threads} threads`
				end)),
			},
			ui.show(function(): boolean
				return #groups(false) > 1
			end, ui.column {
				w_full = true,
				gap = "$space.1",
				ds.Divider {},
				table.unpack(slots(GROUPS, function(i: number): Amount?
					return groups(false)[i]
				end, function(i: number): string
					local g = groups(false)[i]
					return if g then g.name else ""
				end, true)),
			}),
			ui.show(function(): boolean
				return #now().top > 0
			end, ui.column {
				w_full = true,
				gap = "$space.1",
				ds.Divider {},
				table.unpack(slots(TOP, function(i: number): Amount?
					return now().top[i]
				end, function(i: number): string
					local t = now().top[i]
					if not t then
						return ""
					end
					return if t.count > 1 then `{t.name} ×{t.count}` else t.name
				end, false)),
			}),
		}),
		-- Shared with other chats: never in the totals above, or every chat
		-- in a project with a VM would read as the VM.
		ui.show(function(): boolean
			return live() and #groups(true) > 0
		end, ui.column {
			w_full = true,
			gap = "$space.1",
			ds.Divider {},
			text(function(): string
				return "Shared with other chats · not in the totals"
			end, "$font.size.xs", "$text.tertiary", { w_full = true, truncate = true, tabular = false }),
			table.unpack(slots(SHARED, function(i: number): Amount?
				return groups(true)[i]
			end, function(i: number): string
				local g = groups(true)[i]
				return if g then g.name else ""
			end, false)),
		}),
	}
end
