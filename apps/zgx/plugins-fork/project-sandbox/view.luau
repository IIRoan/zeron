--!strict
-- The sandbox tab beside a chat: the project's container, what it reaches
-- and publishes, its build's progress and log, and Rebuild (which asks
-- twice: it stops what runs in the container and takes minutes).
local Button = require("@ds/button")
local IconButton = require("@ds/icon_button").IconButton
local Icon = require("@ds/icon")
local Badge = require("@ds/badge").Badge
local Spinner = require("@ds/status").Spinner

-- How long a first press of Rebuild waits for the second.
local CONFIRM_SECONDS = 6
local LOG_LINES = 600

return function(zeron: any, Model: any, Settings: any)
	local function Fact(label: string, value: () -> string): Node
		return ui.row {
			w_full = true,
			gap = "$space.3",
			items = "start",
			ui.text { label, w = 120, color = "$text.tertiary", text_size = "$font.size.sm" },
			ui.text { value, grow = 1, wrap = true, font = "$font.mono", text_size = "$font.size.sm" },
		}
	end

	local Pane = ui.component(function(p: any): Node
		local m = Model.get(p.context)
		local confirm_until = ui.use_state(0)
		ui.after(0, function()
			m.refresh()
		end)
		local tick = 0
		ui.every(1000, function()
			tick += 1
			if m.building_now() or tick % 5 == 0 then
				m.refresh()
			end
		end)

		local function sandbox(): any?
			local r = m.reply:get()
			return r and r.sandbox
		end
		local function build(): any?
			local r = m.reply:get()
			return r and r.build
		end

		local function pill(): (string, string)
			if m.building() then
				return "Building", "warning"
			end
			local s = sandbox()
			if not s then
				return "Off", "neutral"
			end
			if Model.failed(build()) then
				return "Failed", "danger"
			end
			local state = tostring(s.state or "unknown")
			local word = string.upper(string.sub(state, 1, 1)) .. string.sub(state, 2)
			return word, if state == "running" then "success" else "neutral"
		end

		local function rebuild()
			local now = os.time()
			if confirm_until:peek() < now then
				confirm_until:set(now + CONFIRM_SECONDS)
				ui.after(CONFIRM_SECONDS * 1000, function()
					if confirm_until:peek() <= os.time() then
						confirm_until:set(0)
					end
				end)
				return
			end
			confirm_until:set(0)
			local settings = m.current()
			if settings then
				m.apply(true, settings)
			end
		end

		local function off(): Node
			return ui.column {
				w_full = true,
				gap = "$space.3",
				ui.text {
					function()
						local r = m.reply:get()
						if r and not r.available then
							return "Project sandboxes need Linux and rootless podman on the project's machine."
						end
						return "Agents, services and terminals of this project run on this machine. A sandbox runs them in a container built from the project's devcontainer."
					end,
					wrap = true,
					color = "$text.secondary",
					text_size = "$font.size.sm",
				},
				ui.row {
					Button {
						"Set up",
						tone = "secondary",
						size = "sm",
						disabled = function()
							local r = m.reply:get()
							return r == nil or not r.available
						end,
						on_click = function()
							m.settings:set(true)
						end,
					},
				},
			}
		end

		local function on(): Node
			return ui.column {
				w_full = true,
				gap = "$space.2",
				ui.text {
					function()
						local s = sandbox()
						local name = if s then tostring(s.container or "") else ""
						return if s and s.imageBuilt == false then `{name} · image not built` else name
					end,
					font = "$font.mono",
					text_size = "$font.size.sm",
				},
				Fact("Config", function()
					local s = sandbox()
					return if s then tostring(s.config or "") else ""
				end),
				Fact("Reaches here", function()
					local s = sandbox()
					local ports = zeron.ports.format(s and s.hostPorts)
					return if ports == "" then "No port of this machine" else ports
				end),
				Fact("Published here", function()
					local s = sandbox()
					local ports = zeron.ports.format(s and s.publishPorts)
					return if ports == "" then "Every port a server opens" else ports
				end),
				Fact("Read-only", function()
					local s = sandbox()
					local files = table.concat((s and s.readOnly) or {}, ", ")
					return if files == "" then "None" else files
				end),
				ui.show(function()
					return confirm_until:get() > 0
				end, ui.text {
					"Rebuilding stops the agents, services and terminals running in the container, and takes minutes. Press Rebuild again to go ahead.",
					wrap = true,
					color = "$warning",
					text_size = "$font.size.sm",
				}),
				ui.row {
					gap = "$space.2",
					pt = "$space.1",
					Button {
						function()
							return if confirm_until:get() > 0 then "Rebuild now" else "Rebuild"
						end,
						tone = "secondary",
						size = "sm",
						disabled = m.building,
						on_click = rebuild,
					},
					Button {
						"Settings",
						tone = "ghost",
						size = "sm",
						on_click = function()
							m.settings:set(true)
						end,
					},
				},
			}
		end

		return ui.column {
			w_full = true,
			h_full = true,
			gap = "$space.4",
			p = "$space.4",
			ui.row {
				w_full = true,
				gap = "$space.2",
				items = "center",
				Icon { name = "shield-check", size = 16, color = "$text.secondary" },
				ui.column {
					grow = 1,
					gap = 0,
					ui.text { "Sandbox", weight = "$font.weight.semibold" },
					ui.text { m.context.name, color = "$text.tertiary", text_size = "$font.size.xs" },
				},
				ui.presence(function()
					local word, tone = pill()
					return `{word}|{tone}`
				end, function(key: string): Node
					local word, tone = string.match(key, "^(.*)|(.*)$")
					return Badge { word or "", tone = tone :: any, dot = true }
				end),
				IconButton {
					icon = "settings",
					label = "Sandbox settings",
					size = "sm",
					on_click = function()
						m.settings:set(true)
					end,
				},
			},
			ui.presence(function()
				local r = m.reply:get()
				return if r == nil then "loading" elseif r.sandbox then "on" else "off"
			end, function(kind: string): Node
				if kind == "loading" then
					return ui.text { "Reading the project's sandbox…", color = "$text.tertiary", text_size = "$font.size.sm" }
				end
				return if kind == "on" then on() else off()
			end),
			ui.show(m.building, ui.row {
				w_full = true,
				gap = "$space.2",
				items = "center",
				Spinner { size = 14 },
				ui.text {
					function()
						local b = build()
						return Model.last_step(b and b.log) or "Starting…"
					end,
					grow = 1,
					text_size = "$font.size.sm",
				},
			}),
			ui.show(function()
				return m.error:get() ~= ""
			end, ui.text { m.error, wrap = true, color = "$danger", text_size = "$font.size.sm" }),
			ui.column {
				w_full = true,
				grow = 1,
				min_h = 0,
				gap = "$space.2",
				ui.row {
					w_full = true,
					items = "center",
					ui.text { "Build log", grow = 1, weight = "$font.weight.medium", text_size = "$font.size.sm" },
					ui.text {
						function()
							if m.building() then
								return "Building…"
							end
							return if Model.failed(build()) then "Failed" else "Finished"
						end,
						color = function()
							return if Model.failed(build()) and not m.building() then "$danger" else "$text.tertiary"
						end,
						text_size = "$font.size.xs",
					},
					Button {
						function()
							return if m.log:get() then "Hide" else "Show"
						end,
						tone = "ghost",
						size = "sm",
						on_click = function()
							m.log:set(not m.log:peek())
						end,
					},
				},
				ui.show(m.log, ui.scroll {
					grow = 1,
					min_h = 0,
					w_full = true,
					p = "$space.3",
					rounded = "$radius.md",
					bg = "$surface.sunken",
					ui.text {
						function()
							local b = build()
							local tail = Model.tail(b and b.log, LOG_LINES)
							return if tail == "" then "No output yet" else tail
						end,
						font = "$font.mono",
						text_size = "$font.size.xs",
						wrap = true,
						selectable = true,
					},
				}),
			},
			Settings { model = m },
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
					ui.text { "Open a chat in a project to see its sandbox.", color = "$text.tertiary", text_size = "$font.size.sm" },
				}
			end
			return Pane { context = c }
		end)
	end)
end
