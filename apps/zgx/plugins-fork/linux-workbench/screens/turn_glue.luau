-- (Linux Workbench) A prompt on its way says what it waits for. A chat
-- whose run needs a new worktree waits while the engine prepares it: its
-- project's shared databases ready, its dependencies installed, its setup
-- workflow run. Upstream says "Sending…" through all of it; this says the
-- step, from the project's worktree setup state (the engine's own words
-- for it, `GetWorktreeSettings`), and leaves every other case upstream's.
local original = require("@app-original/screens/chat/turn_glue")
local host = require("@app/screens/settings/host")
local sessions = require("@account/sessions")

local M = table.clone(original)

-- What each local project's setup is doing now ("" nothing), by its id.
local waiting = ui.state("linux_workbench_setup_wait", {} :: { [string]: string })
-- When each was last asked about: polled while it's asked about.
local asked: { [string]: number } = {}
-- Which are being polled now.
local polling: { [string]: boolean } = {}
local POLL_MS = 1500

local function join(names: { string }): string
	if #names <= 1 then
		return names[1] or ""
	end
	return table.concat(names, ", ", 1, #names - 1) .. " and " .. names[#names]
end

-- The setup's step in words, for a snapshot of the project's worktrees.
local function words(snapshot: any): string
	local now: any = nil
	for _, w in snapshot.worktrees or {} do
		if w.state and w.state.phase == "preparing" then
			now = w
		end
	end
	if not now then
		return ""
	end
	local step = tostring(now.state.step or "")
	local doing: string
	if step == "Waiting for project services" then
		local names = {}
		for _, d in snapshot.databases or {} do
			table.insert(names, tostring(d.name))
		end
		doing = if #names > 0 then `waiting for {join(names)} to be ready` else "waiting for the project's services"
	elseif step == "Installing dependencies" then
		local settings = snapshot.settings or {}
		local command = if type(settings.installCommand) == "string" and settings.installCommand ~= ""
			then settings.installCommand
			else snapshot.detectedInstallCommand
		doing = if type(command) == "string" and command ~= "" then `installing dependencies ({command})` else "installing dependencies"
	elseif step ~= "" then
		doing = string.lower(string.sub(step, 1, 1)) .. string.sub(step, 2)
	else
		doing = "setting it up"
	end
	local name = string.match(tostring(now.path or ""), "([^/]+)$") or "a worktree"
	return `Preparing worktree {name}: {doing}…`
end

local function poll(project: string, path: string)
	host.call("settings.engine", { method = "GetWorktreeSettings", params = { spaceId = project, cwd = path } }):after(function(snapshot: any)
		local said = if type(snapshot) == "table" then words(snapshot) else ""
		if waiting:peek()[project] ~= said then
			local next = table.clone(waiting:peek())
			next[project] = said
			waiting:set(next)
		end
		-- (On while a setup runs, or a prompt asked lately; else forgotten,
		-- and asked again from the start.)
		if said == "" and (asked[project] or 0) < os.time() - 5 then
			asked[project] = nil
			polling[project] = nil
			return
		end
		ui.after(POLL_MS, function()
			poll(project, path)
		end)
	end, function()
		ui.after(POLL_MS * 2, function()
			poll(project, path)
		end)
	end)
end

-- What a local project's setup is doing (nil: nothing to say), polled
-- from now on while it's asked.
local function setup_of(project: string): string?
	asked[project] = os.time()
	if not polling[project] then
		local path: string? = nil
		for _, p in sessions.projects:peek() do
			if p.id == project then
				path = p.path
			end
		end
		if path then
			local at = path
			-- (Asked from the transcript's turn state, a computed: a timer
			-- started here lasts only until its next run, which a new chat's
			-- first send brings at once. So a poll counts as started when it
			-- starts, and until then each run arms one again.)
			ui.after(0, function()
				if not polling[project] then
					polling[project] = true
					poll(project, at)
				end
			end)
		end
	end
	local said = waiting:get()[project]
	return if said and said ~= "" then said else nil
end

function M.of(session: () -> any, messages: any?): (m: any, sending: boolean) -> any
	local inner = original.of(session, messages)
	return function(m: any, sending: boolean): any
		local s = session()
		if sending and s and s.project and s.device == sessions.here:get() then
			local said = setup_of(s.project)
			if said then
				return { text = said, spinner = true }
			end
		end
		return inner(m, sending)
	end
end

return M
