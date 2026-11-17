-- Agents driving the browser: the engine's `browser_*` tools, as the Linux
-- desktop app had them (crates/ui/src/shell/browser_control.rs). A tab is
-- its chat's alone, and an agent uses it only while its bar's bot toggle is
-- on: off for a tab a person opens, on for one an agent opens, checked
-- again as each request runs. The host (src/browser_agents.rs) lists the
-- window's tabs to the engine and hands each request here, where the pages
-- are: navigation through the page's controller, reading and filling
-- through the desktop app's page script (`automation`, the host's), and a
-- person's click, typing and keys, and the page's picture, as WebKit takes
-- them (`web:click`, `type`, `key`, `snapshot`).
--
--   Agents.enabled(id)        whether agents may use the tab (tracked)
--   Agents.set_enabled(id, on)
--   Agents.showing(id)        a page view shows the tab: what it returns ends that
--   Agents.Stage {}           boxes, out of sight, for agents' tabs no view shows
--                             (a page exists only while a box shows it)
--   Agents.hooks              screens/panel/browser's: page(id) (the tab's page,
--                             if it has one), make(id) (it, made as the
--                             browser makes them), close(tab), title(tab)

local state = require("@app/screens/panel/state")
local api = require("@app/screens/panel/browser_api")

type Tab = state.Tab

local M = {}

M.hooks = {
	page = function(_id: string): any
		return nil
	end,
	make = function(_id: string): any
		return nil
	end,
	close = function(_tab: Tab) end,
	title = function(_tab: Tab): string?
		return nil
	end,
	titled = function(_id: string, _title: string) end,
}

local enabled = ui.state("browser_agents_enabled", {} :: { [string]: boolean })
local shown = ui.state("browser_agents_shown", {} :: { [string]: number })
-- Requests handed to the page already (kept over a reload: none runs twice).
local taken = ui.state("browser_agents_taken", {} :: { [string]: boolean })
-- The last tab an agent opened (its id `agent<n>`, as the panel's are `tab<n>`).
local opened = ui.state("browser_agents_opened", 0)
local requests = ui.shared("browser_agents.requests", {})

function M.enabled(id: string): boolean
	return enabled:get()[id] == true
end

function M.set_enabled(id: string, on: boolean)
	enabled:set_key(id, if on then true else nil)
end

function M.showing(id: string): () -> ()
	shown:set_key(id, (shown:peek()[id] or 0) + 1)
	return function()
		local n = (shown:peek()[id] or 1) - 1
		shown:set_key(id, if n > 0 then n else nil)
	end
end

-- Every chat's browser tabs (a project's panel has no chat to act for).
local function chat_tabs(sessions: { [string]: any }): { { chat: string, tab: Tab } }
	local out = {}
	for key, s in sessions do
		local chat = string.match(key, "^chat:(.+)$")
		if chat then
			for _, t in s.tabs do
				if t.kind == "browser" then
					table.insert(out, { chat = chat, tab = t })
				end
			end
		end
	end
	return out
end

-- The host's action, where there is one (src/browser_agents.rs: the Linux
-- app's main window; not other platforms, nor tests): the browser works
-- without agents all the same.
local function host(action: string, value: any)
	pcall(ui.invoke, action, value)
end

local function reply(id: string, value: any, err: string?)
	host("browser_agents.reply", { id = id, value = value, error = err })
end

local KEYS = {
	Enter = true, Tab = true, Escape = true, Backspace = true, Delete = true,
	ArrowLeft = true, ArrowRight = true, ArrowUp = true, ArrowDown = true,
	Home = true, End = true, PageUp = true, PageDown = true, Space = true,
}
-- (A PNG of 8 MB, as base64.)
local MOST_PICTURE = math.ceil(8 * 1024 * 1024 / 3) * 4

-- The page's work, once the tab is checked: what the desktop app's page did.
local function page_work(r: any, web: any)
	local c = r.command
	local action = c.action
	if action == "screenshot" then
		web:snapshot(function(png: string?, err: string?)
			if not png then
				reply(r.id, nil, `browser_screenshot_failed: {err or "no picture"}`)
			elseif #png > MOST_PICTURE then
				reply(r.id, nil, "browser_screenshot_failed: image exceeds 8 MB or could not be encoded")
			else
				reply(r.id, { mimeType = "image/png", data = png })
			end
		end)
		return
	end
	if action == "evaluate" then
		web:eval(c.script, function(value: any, err: string?)
			reply(r.id, value, err)
		end)
		return
	end
	if action == "type" then
		web:type(c.text)
		reply(r.id, { ok = true })
		return
	end
	if action == "keypress" then
		local key = c.key
		if not (KEYS[key] or utf8.len(key) == 1) then
			reply(r.id, nil, "browser_invalid_key: use Enter, Tab, Escape, arrow keys, or one character")
			return
		end
		web:key(key)
		reply(r.id, { ok = true })
		return
	end
	-- snapshot, click, fill, select, scroll: the page script.
	web:eval(r.automation, function(value: any, err: string?)
		if err then
			reply(r.id, nil, err)
		elseif type(value) == "table" and type(value.error) == "string" then
			reply(r.id, nil, value.error)
		elseif action == "click" then
			local point = if type(value) == "table" then value.point else nil
			if type(point) ~= "table" or type(point.x) ~= "number" or type(point.y) ~= "number" then
				reply(r.id, nil, "browser_invalid_target")
				return
			end
			web:click(point.x, point.y)
			-- Answered once the page has had the click (it may navigate).
			web:eval("({clicked:true})", function(done: any, e: string?)
				reply(r.id, done, e)
			end)
		else
			reply(r.id, value)
		end
	end)
end

local function run(r: any)
	local key = `chat:{r.chat}`
	local t = state.find(key, r.tab)
	if not t or t.kind ~= "browser" then
		-- (Another chat's tab, or one closed since it was listed.)
		local elsewhere = false
		for _, it in chat_tabs(state.sessions:peek()) do
			if it.tab.id == r.tab then
				elsewhere = true
			end
		end
		reply(r.id, nil, if elsewhere then "browser_denied: tab belongs to another chat" else "browser_closed")
		return
	end
	if not enabled:peek()[t.id] then
		reply(r.id, nil, "browser_disabled: agent control was turned off")
		return
	end
	local c = r.command
	local action = c.action
	if action == "new_tab" then
		if not api.allowed(c.url) then
			reply(r.id, nil, "Only explicit http and https links are supported.")
			return
		end
		-- Beside the chat's others, not in front: the person's tab stays theirs.
		local s = state.sessions:peek()[key]
		local n = opened:peek() + 1
		opened:set(n)
		local id = `agent{n}`
		local tabs = table.clone(s.tabs)
		table.insert(tabs, { id = id, kind = "browser", url = c.url })
		state.sessions:set_key(key, { tabs = tabs, active = s.active, visited = s.visited })
		enabled:set_key(id, true)
		reply(r.id, { tab = id, loading = true })
		return
	end
	if action == "close" then
		M.hooks.close(t)
		state.close(key, t.id)
		enabled:set_key(t.id, nil)
		reply(r.id, { ok = true })
		return
	end
	local web = M.hooks.page(t.id)
	if not web then
		reply(r.id, nil, "browser_unavailable: the tab's page isn't open yet; try again")
		return
	end
	if action == "navigate" then
		if not api.allowed(c.url) then
			reply(r.id, nil, "Only explicit http and https links are supported.")
			return
		end
		state.set_url(key, t.id, c.url)
		web:load(c.url)
		reply(r.id, { ok = true })
		return
	end
	if action == "back" or action == "forward" or action == "reload" then
		if action == "back" then
			web:back()
		elseif action == "forward" then
			web:forward()
		else
			web:reload()
		end
		reply(r.id, { ok = true })
		return
	end
	if web.loading then
		reply(r.id, nil, "browser_loading: wait for the page to finish loading")
		return
	end
	if type(web.error) == "string" and web.error ~= "" then
		reply(r.id, nil, `browser_page_error: {web.error}`)
		return
	end
	page_work(r, web)
end

-- The window's tabs for the engine, as they change.
ui.effect(function()
	local list = {}
	local on = enabled:get()
	for _, it in chat_tabs(state.sessions:get()) do
		local t = it.tab
		local web = M.hooks.page(t.id)
		local url = if web and type(web.url) == "string" and web.url ~= "" then web.url else t.url
		-- (A page's own title: one on agents' stage has no view to tell it.)
		local title = if web and type(web.title) == "string" and web.title ~= "" then web.title else M.hooks.title(t)
		table.insert(list, {
			tab = t.id,
			chat = it.chat,
			title = title or "",
			url = url,
			loading = web ~= nil and web.loading == true,
			enabled = on[t.id] == true,
		})
	end
	host("browser_agents.tabs", list)
end)

-- Each request once, outside the effect (what it reads isn't its key).
ui.effect(function()
	local list = requests:get()
	local seen = taken:peek()
	local live = {}
	for _, r in list do
		live[r.id] = true
		if not seen[r.id] then
			taken:set_key(r.id, true)
			ui.after(0, function()
				local ok, err = pcall(run, r)
				if not ok then
					reply(r.id, nil, `browser_failed: {err}`)
				end
			end)
		end
	end
	-- (Answered ones leave the host's list: forgotten here too.)
	for id in seen do
		if not live[id] then
			taken:set_key(id, nil)
		end
	end
end)

-- An agent's tab no view shows: a box of a desktop window's size, out of
-- sight (transparent, taking no pointer), so its page lives and answers.
local staged = ui.computed(function(): { Tab }
	local on, showing = enabled:get(), shown:get()
	local out = {}
	for _, it in chat_tabs(state.sessions:get()) do
		if on[it.tab.id] and not showing[it.tab.id] then
			table.insert(out, it.tab)
		end
	end
	return out
end)

M.Stage = ui.component(function(): Node
	-- (A page takes its box's size as the box is drawn, and one not drawn
	-- at all, opacity 0, gets none: each page's box has a window's size,
	-- drawn, but the stage shows one pixel of them, all but transparent,
	-- under everything else.)
	return ui.div {
		id = "browser-agents-stage",
		absolute = true,
		left = 0,
		top = 0,
		w = 1,
		h = 1,
		overflow = "hidden",
		opacity = 0.01,
		pointer_events = "none",
		aria_hidden = true,
		ui.each(staged, function(t: Tab): Node
			local web = M.hooks.make(t.id)
			if not web then
				return ui.div {}
			end
			if t.url and not (type(web.url) == "string" and web.url ~= "" and web.url ~= "about:blank") then
				local url = t.url
				ui.after(0, function()
					web:load(url)
				end)
			end
			-- (Its own size: the list's box has none to fill.)
			return api.view {
				web = web,
				absolute = true,
				left = 0,
				top = 0,
				w = 1280,
				h = 800,
				pointer_events = "none",
				-- (The tab's title, as the panel's view would say it.)
				on_title = function(title: string)
					M.hooks.titled(t.id, title)
				end,
			}
		end, { key = function(t: Tab): string
			return t.id
		end }),
	}
end)

return M
