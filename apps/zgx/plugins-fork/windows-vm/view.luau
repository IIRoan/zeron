--!strict
-- The Windows VM tab beside a chat: the project's VM's screen (a picture
-- every few seconds; a click opens the live viewer), its state and health,
-- its ports, and a command to run in it.
local Button = require("@ds/button")
local IconButton = require("@ds/icon_button").IconButton
local Icon = require("@ds/icon")
local Badge = require("@ds/badge").Badge
local Tag = require("@ds/badge").Tag
local fm = require("./fork_motion")
local Spinner = require("@ds/status").Spinner
local TextField = require("@ds/text_field")

local STATES = {
	none = { "Not connected", "neutral" },
	checking = { "Checking", "neutral" },
	running = { "Running", "success" },
	starting = { "Starting", "warning" },
	stopped = { "Stopped", "neutral" },
	unavailable = { "Unavailable", "danger" },
}
local LEVEL_INK = { normal = "$success", warning = "$warning", critical = "$danger" }

return function(zeron: any, Model: any)
	local function Section(title: string, ...: Node?): Node
		return ui.column {
			w_full = true,
			gap = "$space.2",
			p = "$space.3",
			rounded = "$radius.lg",
			border = 1,
			border_color = "$border.subtle",
			ui.text { title, weight = "$font.weight.medium", text_size = "$font.size.sm" },
			...,
		}
	end

	-- A meter: what, how full (0-100), and the numbers behind it.
	local function Meter(label: string, read: () -> (number?, string)): Node
		return ui.column {
			grow = 1,
			basis = 0,
			gap = "$space.1",
			ui.row {
				w_full = true,
				ui.text { label, grow = 1, color = "$text.tertiary", text_size = "$font.size.xs" },
				ui.text {
					function()
						local _, detail = read()
						return detail
					end,
					text_size = "$font.size.xs",
					tabular = true,
				},
			},
			ui.div {
				w_full = true,
				h = 4,
				rounded = 2,
				bg = "$border.subtle",
				ui.div {
					h_full = true,
					rounded = 2,
					w = function()
						local percent = read()
						return `{math.clamp(percent or 0, 0, 100)}%`
					end,
					bg = function()
						local percent = read()
						return LEVEL_INK[Model.level(percent or 0)]
					end,
				},
			},
		}
	end

	-- A list Windows reported, folded until opened, with its count.
	local Fold = ui.component(function(p: any): Node
		local open = ui.use_state(false)
		return ui.column {
			w_full = true,
			gap = "$space.1",
			ui.button {
				w_full = true,
				items = "center",
				gap = "$space.2",
				py = 2,
				label = p.title,
				on_click = function()
					open:set(not open:peek())
				end,
				Icon {
					name = function()
						return if open:get() then "alt-arrow-down" else "alt-arrow-right"
					end :: any,
					size = 12,
					color = "$text.tertiary",
				},
				ui.text { p.title, grow = 1, text_size = "$font.size.sm" },
				ui.text {
					function()
						local n = #p.items()
						return if n == 0 then "None" else tostring(n)
					end,
					color = "$text.tertiary",
					text_size = "$font.size.xs",
				},
			},
			fm.show(open, ui.column {
				w_full = true,
				gap = 2,
				pl = "$space.5",
				ui.text {
					function()
						return table.concat(p.items(), "\n")
					end,
					font = "$font.mono",
					text_size = "$font.size.xs",
					wrap = true,
					selectable = true,
					color = "$text.secondary",
				},
			}),
		}
	end)

	local Pane = ui.component(function(p: any): Node
		local m = Model.get(p.context)
		local command = ui.use_state("")
		ui.after(0, function()
			m.refresh()
		end)
		-- Running: its screen and health at once, then the screen every few
		-- seconds (the health is read again on asking).
		ui.effect(function()
			if m.state() == "running" then
				if not m.screen:peek() then
					m.capture()
				end
				if not m.health:peek() then
					m.diagnose()
				end
			end
		end)
		ui.every(5000, function()
			m.refresh()
			local r = m.reply:peek()
			if r and r.vm and r.vm.state == "running" then
				m.capture()
			end
		end)

		local function vm(): any
			local r = m.reply:get()
			return (r and r.vm) or {}
		end
		local function health(): any
			return m.health:get() or {}
		end
		local function busy(): boolean
			return m.busy:get() ~= ""
		end

		-- What Windows says of itself, in a line.
		local function meta(): string
			local state = m.state()
			if state == "none" then
				return "Not this project's yet"
			end
			local h = m.health:get()
			if not h or not h.os or h.os == "" then
				return "This project's Windows machine"
			end
			local parts = { tostring(h.os) }
			if h.uptimeSeconds then
				table.insert(parts, `up {Model.duration(h.uptimeSeconds)}`)
			end
			table.insert(parts, if h.loggedOnUser and h.loggedOnUser ~= "" then `{h.loggedOnUser} signed in` else "nobody signed in")
			return table.concat(parts, " · ")
		end

		local function screen(): Node
			return ui.presence(function()
				local state = m.state()
				if state == "running" then
					return if m.screen:get() then "picture" else "capturing"
				end
				return state
			end, function(kind: string): Node
				local frame = {
					w_full = true,
					min_h = 180,
					rounded = "$radius.lg",
					bg = "$surface.sunken",
					items = "center",
					justify = "center",
					gap = "$space.2",
				}
				local function say(icon: string, text: () -> string, action: Node?): Node
					local box: any = table.clone(frame)
					table.insert(box, Icon { name = icon :: any, size = 22, color = "$text.tertiary" })
					table.insert(box, ui.text { text, color = "$text.secondary", text_size = "$font.size.sm", wrap = true })
					if action then
						table.insert(box, action)
					end
					return ui.column(box)
				end
				if kind == "picture" then
					return ui.button {
						w_full = true,
						rounded = "$radius.lg",
						-- A picture of the screen, taken again every few seconds.
						label = "Windows' screen: open the live viewer",
						on_click = function()
							m.control("view")
						end,
						ui.image {
							src = function()
								local s = m.screen:get()
								return if s then s.src else ""
							end,
							w_full = true,
							aspect = function()
								local s = m.screen:get()
								return if s and s.height and s.height > 0 then math.max(s.width / s.height, 1.6) else 1.6
							end,
							fit = "contain",
							rounded = "$radius.lg",
						},
					}
				elseif kind == "capturing" then
					return say("monitor", function()
						local e = m.screen_error:get()
						return if e ~= "" then e else "Capturing the screen…"
					end)
				elseif kind == "starting" then
					return say("monitor", function()
						return "Windows is starting…"
					end)
				elseif kind == "stopped" then
					return say("monitor", function()
						return "Windows is off"
					end, Button {
						"Start VM",
						tone = "secondary",
						size = "sm",
						disabled = busy,
						on_click = function()
							m.control("start")
						end,
					})
				elseif kind == "none" then
					return say("monitor", function()
						local r = m.reply:get()
						if r and not r.available then
							return "The Windows VM isn't set up on this machine yet (scripts/winvm)."
						end
						return "Connect this project to the Windows VM to give its agents a Windows machine."
					end, Button {
						"Connect in settings",
						tone = "secondary",
						size = "sm",
						on_click = function()
							zeron.open { settings = `project:{m.context.project}` }
						end,
					})
				elseif kind == "checking" then
					return say("monitor", function()
						return "Checking the VM…"
					end)
				end
				return say("danger-triangle", function()
					local e = vm().error
					return if e and e ~= "" then e else "The VM can't be used on this machine yet."
				end)
			end, fm.swap { w_full = true })
		end

		local function ports(title: string, list: () -> { number }?, empty: () -> string): Node
			return ui.column {
				w_full = true,
				gap = "$space.1",
				ui.text { title, color = "$text.tertiary", text_size = "$font.size.xs" },
				ui.presence(function()
					return zeron.ports.format(list())
				end, function(text: string): Node
					if text == "" then
						return ui.text { empty, color = "$text.secondary", text_size = "$font.size.sm" }
					end
					local tags = { gap = "$space.1", wrap = true }
					for _, port in zeron.words(text) do
						table.insert(tags, Tag { port, size = "sm" })
					end
					return ui.row(tags)
				end, fm.swap()),
			}
		end

		local function machine(): Node
			return ui.column {
				w_full = true,
				gap = "$space.3",
				ui.row {
					w_full = true,
					gap = "$space.3",
					Meter("CPU", function()
						local h = health()
						local load = h.cpuLoadPercent
						return load, if load then `{math.floor(load)}%` else "–"
					end),
					Meter("Memory", function()
						local memory = health().memoryMB
						if not memory or not memory.total or memory.total <= 0 then
							return nil, "–"
						end
						local used = memory.total - (memory.free or 0)
						return 100 * used / memory.total, `{math.floor(used / 1024 * 10) / 10} / {math.floor(memory.total / 1024 * 10) / 10} GB`
					end),
					Meter("Disk C:", function()
						local disk = health().diskGB
						if not disk or not disk.total or disk.total <= 0 then
							return nil, "–"
						end
						local used = disk.total - (disk.free or 0)
						return 100 * used / disk.total, `{math.floor(used)} / {math.floor(disk.total)} GB`
					end),
				},
				ports("Ports in Windows", function()
					return vm().toVm
				end, function()
					local r = m.reply:get()
					local auto = not (r and r.settings and r.settings.autoPorts == false)
					return if auto then "None yet · this project's servers connect by themselves" else "None · list them in settings"
				end),
				ports("Ports from Windows", function()
					local r = m.reply:get()
					return r and r.settings and r.settings.fromVm
				end, function()
					return "None"
				end),
				fm.show(function()
					local e = vm().tunnelError
					return e ~= nil and e ~= ""
				end, ui.text {
					function()
						return tostring(vm().tunnelError or "")
					end,
					wrap = true,
					color = "$warning",
					text_size = "$font.size.sm",
				}),
				ui.row {
					gap = "$space.2",
					Button {
						"Open viewer",
						tone = "secondary",
						size = "sm",
						disabled = busy,
						on_click = function()
							m.control("view")
						end,
					},
					Button {
						"Reconnect ports",
						tone = "secondary",
						size = "sm",
						disabled = busy,
						on_click = function()
							m.control("start")
						end,
					},
					Button {
						"Shut down",
						tone = "ghost",
						size = "sm",
						disabled = busy,
						on_click = function()
							m.control("stop")
						end,
					},
				},
			}
		end

		local function run(): Node
			local function go(action: string)
				local text = command:peek()
				if string.find(text, "%S") then
					m.control(action, text)
				end
			end
			return Section(
				"Run in Windows",
				TextField {
					value = command,
					placeholder = "hostname",
					w_full = true,
					font = "$font.mono",
					-- Enter runs it in PowerShell.
					on_commit = function()
						go("exec")
					end,
				},
				ui.row {
					gap = "$space.2",
					Button {
						"Run in PowerShell",
						tone = "secondary",
						size = "sm",
						busy = function()
							return m.busy:get() == "exec"
						end,
						disabled = busy,
						on_click = function()
							go("exec")
						end,
					},
					Button {
						"Start on the desktop",
						tone = "ghost",
						size = "sm",
						disabled = busy,
						on_click = function()
							go("launch")
						end,
					},
				},
				ui.presence(function()
					local ran = m.ran:get()
					return if ran then `{ran.kind}:{ran.command}:{tostring(ran.result and ran.result.exitCode)}` else ""
				end, function(key: string): Node
					local ran = m.ran:peek()
					if key == "" or not ran then
						return ui.div {}
					end
					if ran.kind == "launch" then
						return ui.text { `Started on the Windows desktop: {ran.command}…`, color = "$text.secondary", text_size = "$font.size.sm", wrap = true }
					end
					local result = ran.result or {}
					local out = table.concat({ result.stdout or "", result.stderr or "" }, "")
					local ok = result.exitCode == 0 and not result.timedOut
					return ui.column {
						w_full = true,
						gap = "$space.1",
						ui.row {
							w_full = true,
							gap = "$space.2",
							items = "center",
							ui.div { w = 8, h = 8, rounded = 4, bg = if ok then "$success" else "$danger" },
							ui.text { `{Model.ran(result)} · {ran.command}`, grow = 1, text_size = "$font.size.sm" },
							Button {
								"Copy",
								tone = "ghost",
								size = "sm",
								on_click = function()
									ui.clipboard.write(out)
								end,
							},
						},
						ui.scroll {
							w_full = true,
							max_h = 220,
							p = "$space.2",
							rounded = "$radius.md",
							bg = "$surface.sunken",
							ui.text {
								if out == "" then "It printed nothing." else out,
								font = "$font.mono",
								text_size = "$font.size.xs",
								wrap = true,
								selectable = true,
							},
						},
					}
				end, fm.swap { w_full = true })
			)
		end

		local function healthy(): Node
			local function list(key: string, shape: ((any) -> string)?): () -> { string }
				return function()
					local out = {}
					for _, item in health()[key] or {} do
						table.insert(out, if shape then shape(item) else tostring(item))
					end
					return out
				end
			end
			return Section(
				"Health",
				ui.row {
					w_full = true,
					items = "center",
					gap = "$space.2",
					ui.text {
						function()
							local at = m.health_at:get()
							if m.reading:get() then
								return "Reading Windows' health…"
							end
							return if at == 0 then "No reading yet." else `Read {Model.duration(os.time() - at)} ago`
						end,
						grow = 1,
						color = "$text.tertiary",
						text_size = "$font.size.xs",
					},
					IconButton { icon = "refresh", label = "Read again", size = "sm", disabled = m.reading, on_click = m.diagnose },
				},
				ui.text {
					function()
						local d = health().display
						return if d then `Display · {d}` else ""
					end,
					text_size = "$font.size.sm",
					color = "$text.secondary",
				},
				Fold {
					title = "Biggest processes",
					items = list("topProcesses", function(p: any): string
						return `{p.name} · {p.pid} · {math.floor(p.memoryMB or 0)} MB`
					end),
				},
				Fold { title = "Listening ports", items = list("listeningPorts") },
				Fold { title = "Automatic services that are stopped", items = list("stoppedAutomaticServices") },
				Fold { title = "Errors in the last hour", items = list("recentErrors") }
			)
		end

		return ui.scroll {
			w_full = true,
			h_full = true,
			ui.column {
				w_full = true,
				gap = "$space.4",
				p = "$space.4",
				ui.row {
					w_full = true,
					gap = "$space.2",
					items = "center",
					Icon { name = "monitor", size = 16, color = "$text.secondary" },
					ui.column {
						grow = 1,
						gap = 0,
						ui.text { "Windows VM", weight = "$font.weight.semibold" },
						ui.text { meta, color = "$text.tertiary", text_size = "$font.size.xs" },
					},
					ui.presence(function()
						return m.state()
					end, function(state: string): Node
						local look = (STATES :: any)[state] or STATES.unavailable
						return Badge { look[1], tone = look[2], dot = true }
					end, fm.swap()),
					IconButton {
						icon = "settings",
						label = "Windows VM settings",
						size = "sm",
						on_click = function()
							zeron.open { settings = `project:{m.context.project}` }
						end,
					},
				},
				screen(),
				fm.show(function()
					local s = m.state()
					return s == "running" or s == "starting"
				end, machine()),
				fm.show(busy, ui.row {
					gap = "$space.2",
					items = "center",
					Spinner { size = 14 },
					ui.text {
						function()
							local words = { start = "Connecting ports…", stop = "Shutting Windows down…", view = "Opening the viewer…", exec = "Running…", launch = "Starting it…", save = "Saving…", disconnect = "Disconnecting…" }
							return (words :: any)[m.busy:get()] or "Working…"
						end,
						text_size = "$font.size.sm",
					},
				}),
				fm.show(function()
					return m.error:get() ~= ""
				end, ui.text { m.error, wrap = true, color = "$danger", text_size = "$font.size.sm" }),
				fm.show(function()
					return m.state() == "running"
				end, run()),
				fm.show(function()
					return m.state() == "running"
				end, healthy()),
				},
		}
	end)

	return ui.component(function(p: any): Node
		return ui.presence(function()
			local c = Model.context(p.session, p.project)
			return if c then c.key else ""
		end, function(key: string): Node
			local c = Model.context(p.session, p.project)
			if key == "" or not c then
				return ui.column {
					w_full = true,
					p = "$space.4",
					ui.text { "Open a chat in a project to see its Windows VM.", color = "$text.tertiary", text_size = "$font.size.sm" },
				}
			end
			return Pane { context = c }
		end, fm.swap { w_full = true, h_full = true })
	end)
end
