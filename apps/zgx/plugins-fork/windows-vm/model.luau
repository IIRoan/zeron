--!strict
-- A project's Windows VM, as the engine reports it (`GetProjectWindowsVm`):
--   { available, settings = { root, toVm, fromVm, viewer, startWith,
--     autoPorts }?, vm = { state = "stopped" | "starting" | "running" |
--     "unavailable", toVm, fromVm, viewer, tunnelError?, error? }?, result }
-- and what `ControlProjectWindowsVm` does with it: start, stop, view (the
-- viewer window on the project's machine), diagnostics, screenshot (a
-- `host:` image the host decodes), exec (PowerShell) and launch.
return function(zeron: any)
	local M = {}
	local models: { [string]: any } = {}

	function M.message(err: any): string
		return if type(err) == "table" and err.message then tostring(err.message) else tostring(err)
	end

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

	-- A count of seconds as the person says it: "3 h 20 min", "5 min".
	function M.duration(seconds: number?): string
		local s = math.floor(seconds or 0)
		local days, hours, minutes = s // 86400, (s % 86400) // 3600, (s % 3600) // 60
		if days > 0 then
			return `{days} d {hours} h`
		elseif hours > 0 then
			return `{hours} h {minutes} min`
		end
		return `{math.max(minutes, 0)} min`
	end

	-- How full something is (0-100), and how worried to be about it.
	function M.level(percent: number): string
		return if percent >= 95 then "critical" elseif percent >= 80 then "warning" else "normal"
	end

	-- What an `exec` reply says happened.
	function M.ran(result: any): string
		if result.timedOut then
			return "Stopped · it ran longer than two minutes"
		elseif result.exitCode == nil then
			return "Ended without an exit code"
		elseif result.exitCode == 0 then
			return "Finished · exit code 0"
		end
		return `Failed · exit code {result.exitCode}`
	end

	function M.get(context: any): any
		if models[context.key] then
			return models[context.key]
		end
		local key = context.key
		local m: any = {
			context = context,
			reply = ui.state(`windows-vm.{key}.reply`, nil :: any),
			error = ui.state(`windows-vm.{key}.error`, ""),
			-- The action running now ("save", "disconnect", "start", "stop",
			-- "view", "exec", "launch"), or "".
			busy = ui.state(`windows-vm.{key}.busy`, ""),
			screen = ui.state(`windows-vm.{key}.screen`, nil :: any),
			screen_error = ui.state(`windows-vm.{key}.screen-error`, ""),
			health = ui.state(`windows-vm.{key}.health`, nil :: any),
			health_at = ui.state(`windows-vm.{key}.health-at`, 0),
			reading = ui.state(`windows-vm.{key}.reading`, false),
			ran = ui.state(`windows-vm.{key}.ran`, nil :: any),
			settings = ui.state(`windows-vm.{key}.settings`, false),
			loading = false,
			capturing = false,
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
			m.call("GetProjectWindowsVm", { brief = false }):after(function(reply: any)
				m.loading = false
				m.reply:set(reply)
			end, function(err: any)
				m.loading = false
				m.error:set(M.message(err))
			end)
		end

		function m.connected(): boolean
			local r = m.reply:get()
			return r ~= nil and r.settings ~= nil
		end

		-- The VM's state: "none" (not this project's), "checking",
		-- "running", "starting", "stopped" or "unavailable".
		function m.state(): string
			local r = m.reply:get()
			if not r then
				return "checking"
			elseif not r.settings then
				return "none"
			elseif not r.vm then
				return "checking"
			end
			return tostring(r.vm.state or "unavailable")
		end

		function m.save(settings: any, done: (() -> ())?)
			if m.busy:peek() ~= "" then
				return
			end
			m.busy:set("save")
			m.error:set("")
			local params = table.clone(settings)
			params.enabled = true
			m.call("SetProjectWindowsVm", params):after(function(reply: any)
				m.busy:set("")
				m.reply:set(reply)
				if done then
					done()
				end
			end, function(err: any)
				m.busy:set("")
				m.error:set(M.message(err))
			end)
		end

		function m.disconnect()
			if m.busy:peek() ~= "" then
				return
			end
			m.busy:set("disconnect")
			m.error:set("")
			m.call("SetProjectWindowsVm", { enabled = false }):after(function(reply: any)
				m.busy:set("")
				m.reply:set(reply)
				m.screen:set(nil)
				m.health:set(nil)
			end, function(err: any)
				m.busy:set("")
				m.error:set(M.message(err))
			end)
		end

		-- start, stop, view, exec, launch: one at a time.
		function m.control(action: string, command: string?)
			if m.busy:peek() ~= "" then
				return
			end
			m.busy:set(action)
			m.error:set("")
			m.call("ControlProjectWindowsVm", { action = action, command = command or ui.null }):after(function(reply: any)
				m.busy:set("")
				if reply.settings ~= nil or reply.vm ~= nil then
					m.reply:set(reply)
				end
				if action == "exec" or action == "launch" then
					m.ran:set({ kind = action, command = command or "", result = reply.result })
				end
				if action == "stop" then
					m.screen:set(nil)
				end
				m.refresh()
			end, function(err: any)
				m.busy:set("")
				m.error:set(M.message(err))
				m.refresh()
			end)
		end

		-- The screen now: the host keeps the picture (`host:` src), replaced
		-- in place on the next one, so what shows it follows.
		function m.capture()
			if m.capturing then
				return
			end
			m.capturing = true
			m.call("ControlProjectWindowsVm", { action = "screenshot" }):after(function(reply: any)
				m.capturing = false
				m.screen_error:set("")
				if reply.result and reply.result.src then
					m.screen:set(reply.result)
				end
			end, function(err: any)
				m.capturing = false
				m.screen_error:set(M.message(err))
			end)
		end

		-- Windows' health (`winvm diag`): edition, uptime, who's signed in,
		-- load, memory, disk, display, and what's large, listening, stopped
		-- or failing.
		function m.diagnose()
			if m.reading:peek() then
				return
			end
			m.reading:set(true)
			m.call("ControlProjectWindowsVm", { action = "diagnostics" }):after(function(reply: any)
				m.reading:set(false)
				m.health:set(reply.result)
				m.health_at:set(os.time())
			end, function(err: any)
				m.reading:set(false)
				m.error:set(M.message(err))
			end)
		end

		return m
	end

	return M
end
