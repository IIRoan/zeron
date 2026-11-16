--!strict
-- A project's sandbox, as the engine reports it (`GetProjectSandbox`):
--   { available, sandbox = Status?, build = { running, log }, suggestedConfig? }
-- Status: { root, config, hostPorts, publishPorts, readOnly, shareGithub,
--   signCommits, container, state, imageBuilt, ... }.
-- One model per project, shared by its settings sheet and its panel tab.
local STEP = "▸"

return function(zeron: any)
	local M = {}
	local models: { [string]: any } = {}

	function M.message(err: any): string
		return if type(err) == "table" and err.message then tostring(err.message) else tostring(err)
	end

	-- A project's context: the scope every call holds to (the project's
	-- device and folder), from a chat in it or the project itself.
	function M.context(session: string?, project: string?): any?
		local chat = if session then zeron.context_of("session", session) else nil
		project = (chat and chat.project) or project
		local p = if project then zeron.context_of("project", project) else nil
		if not p or not p.path or not p.device then
			return nil
		end
		return {
			project = project,
			name = p.name or "Project",
			root = p.path,
			scope = { device = p.device, project = project, cwd = p.path },
			key = `{p.device}:{project}`,
		}
	end

	-- The build log's steps ("▸ Building the image …"); the last says where
	-- it is, "▸ Failed: …" that it stopped.
	function M.last_step(log: string?): string?
		local last = nil
		for line in string.gmatch(log or "", "[^\n]+") do
			if string.sub(line, 1, #STEP) == STEP then
				last = line
			end
		end
		return last
	end

	function M.failed(build: any?): boolean
		local step = M.last_step(build and build.log)
		return step ~= nil and string.find(step, `^{STEP} Failed`) ~= nil
	end

	-- The log's last line with something on it, for a one-line progress.
	function M.latest(log: string?): string?
		local last = nil
		for line in string.gmatch(log or "", "[^\n]+") do
			if string.find(line, "%S") then
				last = line
			end
		end
		return last
	end

	-- Its last `n` lines (a long build writes thousands).
	function M.tail(log: string?, n: number): string
		local lines = {}
		for line in string.gmatch(log or "", "[^\n]*\n?") do
			if line ~= "" then
				table.insert(lines, (string.gsub(line, "\n$", "")))
			end
		end
		local from = math.max(1, #lines - n + 1)
		return table.concat(lines, "\n", from, #lines)
	end

	function M.get(context: any): any
		if models[context.key] then
			return models[context.key]
		end
		local key = context.key
		local m: any = {
			context = context,
			reply = ui.state(`sandbox.{key}.reply`, nil :: any),
			error = ui.state(`sandbox.{key}.error`, ""),
			-- "" | "on" | "off": a SetProjectSandbox still running (a build
			-- takes minutes; it carries on if its sheet closes).
			requesting = ui.state(`sandbox.{key}.requesting`, ""),
			settings = ui.state(`sandbox.{key}.settings`, false),
			log = ui.state(`sandbox.{key}.log`, false),
			loading = false,
		}
		models[key] = m

		function m.call(method: string, params: any?): any
			return zeron.workbench(method, context, params or {})
		end

		function m.refresh()
			if m.loading then
				return
			end
			m.loading = true
			m.call("GetProjectSandbox"):after(function(reply: any)
				m.loading = false
				m.reply:set(reply)
			end, function(err: any)
				m.loading = false
				m.error:set(M.message(err))
			end)
		end

		-- Building (or turning off) now, read reactively.
		function m.building(): boolean
			local r = m.reply:get()
			return m.requesting:get() ~= "" or (r ~= nil and r.build ~= nil and r.build.running == true)
		end
		-- The same, for timers.
		function m.building_now(): boolean
			local r = m.reply:peek()
			return m.requesting:peek() ~= "" or (r ~= nil and r.build ~= nil and r.build.running == true)
		end

		function m.on(): boolean
			local r = m.reply:get()
			return r ~= nil and r.sandbox ~= nil
		end

		-- The settings it runs with now, to build again with (Rebuild).
		function m.current(): any?
			local r = m.reply:peek()
			local s = r and r.sandbox
			if not s then
				return nil
			end
			return {
				config = s.config,
				hostPorts = s.hostPorts or {},
				publishPorts = s.publishPorts or {},
				readOnly = s.readOnly or {},
				shareGithub = s.shareGithub == true,
				signCommits = s.signCommits == true,
			}
		end

		-- Build and start it with `settings` (enabled), or remove it. The
		-- engine answers once the container runs, or fails, minutes later.
		function m.apply(enabled: boolean, settings: any?, done: (() -> ())?)
			if m.requesting:peek() ~= "" then
				return
			end
			m.requesting:set(if enabled then "on" else "off")
			m.error:set("")
			local params = if settings then table.clone(settings) else {}
			params.enabled = enabled
			m.call("SetProjectSandbox", params):after(function(reply: any)
				m.requesting:set("")
				m.reply:set(reply)
				if not enabled then
					zeron.toast(`Sandbox off · {context.name}`)
				elseif M.failed(reply.build) then
					zeron.toast(`Sandbox build failed · {context.name}`)
					m.log:set(true)
				else
					zeron.toast(`Sandbox ready · {context.name}`)
				end
				if done then
					done()
				end
			end, function(err: any)
				m.requesting:set("")
				m.error:set(M.message(err))
				if enabled then
					zeron.toast(`Sandbox build failed · {context.name}`)
				end
				m.refresh()
			end)
		end

		return m
	end

	return M
end
