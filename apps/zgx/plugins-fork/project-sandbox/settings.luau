--!strict
-- The project's sandbox settings, a section of its page in Settings →
-- Projects: the devcontainer config, the ports either way, read-only files
-- and the logins it may use. Applying builds the image and starts the
-- container (minutes); turning it off removes the container. Both carry on
-- if the page closes.
local Button = require("@ds/button")
local TextField = require("@ds/text_field")
local Switch = require("@ds/switch")
local Settings = require("@ds/settings")
local Spinner = require("@ds/status").Spinner

local function trim(text: string): string
	return (string.gsub(text, "^%s*(.-)%s*$", "%1"))
end

return function(zeron: any, Model: any)
	return ui.component(function(p: any): Node
		local m = p.model
		local show_log = ui.use_state(false)
		local config = ui.use_state("")
		local host_ports = ui.use_state("")
		local publish_ports = ui.use_state("")
		local read_only = ui.use_state("")
		local share_github = ui.use_state(false)
		local sign_commits = ui.use_state(false)
		local problem = ui.use_state("")
		-- The form is filled once the reply comes (and after an apply
		-- answers), never under the person's typing.
		local filled = false

		local function fill(reply: any)
			local s = reply.sandbox
			config:set(if s then s.config or "" else reply.suggestedConfig or "")
			host_ports:set(zeron.ports.format(s and s.hostPorts))
			publish_ports:set(zeron.ports.format(s and s.publishPorts))
			read_only:set(table.concat((s and s.readOnly) or {}, ", "))
			share_github:set(s ~= nil and s.shareGithub == true)
			sign_commits:set(s ~= nil and s.signCommits == true)
		end

		ui.after(0, function()
			m.error:set("")
			m.refresh()
		end)
		ui.effect(function()
			local reply = m.reply:get()
			if reply and not filled then
				filled = true
				fill(reply)
			end
		end)
		-- While it builds, its progress; otherwise now and then.
		local tick = 0
		ui.every(1000, function()
			tick += 1
			if m.building_now() or tick % 5 == 0 then
				m.refresh()
			end
		end)

		-- What the form says, or nil and why not.
		local function settings(): any?
			problem:set("")
			local path = trim(config:peek())
			if string.sub(path, 1, 1) ~= "/" then
				problem:set("Devcontainer config: give its full path")
				return nil
			end
			local host, host_error = zeron.ports.parse(host_ports:peek())
			if not host then
				problem:set(`Host ports: {host_error}`)
				return nil
			end
			local publish, publish_error = zeron.ports.parse(publish_ports:peek())
			if not publish then
				problem:set(`Published ports: {publish_error}`)
				return nil
			end
			return {
				config = path,
				hostPorts = host,
				publishPorts = publish,
				readOnly = zeron.words(read_only:peek()),
				shareGithub = share_github:peek(),
				signCommits = sign_commits:peek(),
			}
		end

		local function apply()
			local s = settings()
			if s then
				m.apply(true, s, function()
					filled = false
				end)
			end
		end

		local function status(): string
			local r = m.reply:get()
			if not r then
				return "Reading the project's sandbox…"
			end
			local s = r.sandbox
			if not s then
				return if r.available
					then "Off · agents run on this machine."
					else "Project sandboxes need Linux and rootless podman on the project's machine."
			end
			local built = if s.imageBuilt == false then " · image not built" else ""
			return `On · container {s.state or "unknown"}{built}. Agents, services and terminals of this project run in it.`
		end

		local function busy_text(): string
			local r = m.requesting:get()
			return if r == "off" then "Turning the sandbox off…" else "Building the image and starting the container…"
		end

		local function field(label: string, value: any, placeholder: string, hint: string): Node
			return Settings.Row {
				label = label,
				description = hint,
				stacked = true,
				control = TextField { value = value, name = label, placeholder = placeholder, w_full = true, font = "$font.mono" },
			}
		end

		local function switch(label: string, value: any, hint: string): Node
			return Settings.Row { label = label, description = hint, control = Switch { value = value, label = label } }
		end

		local blocked = function(): boolean
			return m.building() or m.reply:get() == nil
		end

		return Settings.Section {
			title = "Sandbox",
			description = "Run this project's agents, services and terminals in a container built from its devcontainer.",
			ui.column {
				id = "sandbox-settings",
				w_full = true,
				gap = "$space.3",
				ui.text { status, px = Settings.inset(), wrap = true, color = "$text.secondary", text_size = "$font.size.sm" },
				Settings.Group {
					field("Devcontainer config", config, "/path/to/.devcontainer/devcontainer.json", "The image is built from it. Give its full path."),
					field("Host ports", host_ports, "5432, 6379", "Ports of this machine the container may reach (a database here)."),
					field("Published ports", publish_ports, "Automatic", "Ports of the container this machine reaches. Empty: every port a server opens."),
					field("Read-only files", read_only, "compose.yaml, bunfig.toml", "Files of the project its agents may read but not change."),
					switch("Share GitHub login", share_github, "Takes effect when you apply."),
					switch("Sign commits", sign_commits, "Takes effect when you apply."),
				},
				ui.show(m.building, ui.column {
					w_full = true,
					px = Settings.inset(),
					gap = "$space.1",
					ui.row { gap = "$space.2", items = "center", Spinner { size = 14 }, ui.text { busy_text, text_size = "$font.size.sm" } },
					ui.text {
						function()
							local r = m.reply:get()
							return Model.latest(r and r.build and r.build.log) or "Starting…"
						end,
						font = "$font.mono",
						text_size = "$font.size.xs",
						color = "$text.tertiary",
						wrap = true,
					},
				}),
				ui.show(show_log, ui.scroll {
					w_full = true,
					max_h = 280,
					p = "$space.3",
					rounded = "$radius.md",
					bg = "$surface.sunken",
					ui.text {
						function()
							local r = m.reply:get()
							local tail = Model.tail(r and r.build and r.build.log, 600)
							return if tail == "" then "No output yet" else tail
						end,
						font = "$font.mono",
						text_size = "$font.size.xs",
						wrap = true,
						selectable = true,
					},
				}),
				ui.show(function()
					return problem:get() ~= "" or m.error:get() ~= ""
				end, ui.text {
					function()
						return if problem:get() ~= "" then problem:get() else m.error:get()
					end,
					px = Settings.inset(),
					color = "$danger",
					wrap = true,
					text_size = "$font.size.sm",
				}),
				ui.row {
					w_full = true,
					px = Settings.inset(),
					gap = "$space.2",
					items = "center",
					ui.text {
						"Applying builds the image, which can take minutes. It keeps going if you leave this page.",
						grow = 1,
						basis = 0,
						wrap = true,
						color = "$text.tertiary",
						text_size = "$font.size.xs",
					},
					ui.show(function()
						local r = m.reply:get()
						return r ~= nil and r.build ~= nil and (r.build.log or "") ~= ""
					end, Button {
						function()
							return if show_log:get() then "Hide log" else "Show log"
						end,
						tone = "ghost",
						size = "sm",
						on_click = function()
							show_log:set(not show_log:peek())
						end,
					}),
					ui.show(function()
						return m.on()
					end, Button {
						"Turn off",
						tone = "secondary",
						size = "sm",
						disabled = blocked,
						on_click = function()
							m.apply(false, nil, function()
								filled = false
							end)
						end,
					}),
					Button {
						function()
							return if m.on() then "Apply and rebuild" else "Turn on"
						end,
						size = "sm",
						busy = function()
							return m.requesting:get() == "on"
						end,
						disabled = function()
							local r = m.reply:get()
							return blocked() or (r ~= nil and not r.available and r.sandbox == nil)
						end,
						on_click = apply,
					},
				},
			},
		}
	end)
end
