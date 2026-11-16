--!strict
-- The project's sandbox settings: a sheet with the devcontainer config, the
-- ports either way, read-only files and the logins it may use. Applying
-- builds the image and starts the container (minutes); turning it off
-- removes the container. Both carry on if the sheet closes.
local Button = require("@ds/button")
local TextField = require("@ds/text_field")
local Switch = require("@ds/switch")
local Sheet = require("@ds/dialog").Sheet
local Spinner = require("@ds/status").Spinner

local function trim(text: string): string
	return (string.gsub(text, "^%s*(.-)%s*$", "%1"))
end

return function(zeron: any, Model: any)
	local function Note(text: string): Node
		return ui.text { text, wrap = true, color = "$text.tertiary", text_size = "$font.size.xs" }
	end

	return ui.component(function(p: any): Node
		local m = p.model
		-- Its own open state where it's placed more than once (the project's
		-- actions and the tab), its model shared: a build started in one
		-- shows in the other.
		local open = p.open or m.settings
		local show_log = ui.use_state(false)
		local config = ui.use_state("")
		local host_ports = ui.use_state("")
		local publish_ports = ui.use_state("")
		local read_only = ui.use_state("")
		local share_github = ui.use_state(false)
		local sign_commits = ui.use_state(false)
		local problem = ui.use_state("")
		-- The form is filled once a sheet opens (and after an apply answers),
		-- never under the person's typing.
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

		local was = false
		ui.effect(function()
			local showing = open:get()
			if showing and not was then
				filled = false
				problem:set("")
				m.error:set("")
				m.refresh()
			end
			was = showing
		end)
		ui.effect(function()
			local reply = m.reply:get()
			if open:get() and reply and not filled then
				filled = true
				fill(reply)
			end
		end)
		-- While it builds, its progress; otherwise now and then.
		local tick = 0
		ui.every(1000, function()
			tick += 1
			if open:peek() and (m.building_now() or tick % 5 == 0) then
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

		local function field(label: string, value: any, placeholder: string, mono: boolean?, hint: string?): Node
			return ui.column {
				gap = "$space.1",
				w_full = true,
				TextField {
					value = value,
					label = label,
					placeholder = placeholder,
					w_full = true,
					font = if mono then "$font.mono" else nil,
				},
				if hint then Note(hint) else nil,
			}
		end

		local function switch(label: string, value: any): Node
			return ui.row {
				w_full = true,
				items = "center",
				gap = "$space.3",
				ui.column {
					grow = 1,
					gap = 2,
					ui.text { label, text_size = "$font.size.sm" },
					Note("Takes effect when you apply."),
				},
				Switch { value = value, label = label },
			}
		end

		local blocked = function(): boolean
			return m.building() or m.reply:get() == nil
		end

		return Sheet {
			open = open,
			size = 560,
			title = function()
				return `Sandbox · {m.context.name}`
			end,
			foot = ui.row {
				w_full = true,
				gap = "$space.2",
				items = "center",
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
				ui.div { grow = 1 },
				ui.show(function()
					return m.on()
				end, Button {
					"Turn off",
					tone = "secondary",
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
			ui.column {
				w_full = true,
				gap = "$space.4",
				ui.column {
					gap = "$space.1",
					ui.text { "Run this project in a container", weight = "$font.weight.semibold" },
					ui.text { status, wrap = true, color = "$text.secondary", text_size = "$font.size.sm" },
				},
				field(
					"Devcontainer config",
					config,
					"/path/to/.devcontainer/devcontainer.json",
					true,
					"The image is built from it. Give its full path."
				),
				field("Host ports", host_ports, "5432, 6379", true, "Ports of this machine the container may reach (a database here)."),
				field(
					"Published ports",
					publish_ports,
					"Automatic",
					true,
					"Ports of the container this machine reaches. Empty: every port a server opens."
				),
				field("Read-only files", read_only, "compose.yaml, bunfig.toml", true, "Files of the project its agents may read but not change."),
				switch("Share GitHub login", share_github),
				switch("Sign commits", sign_commits),
				ui.show(m.building, ui.column {
					w_full = true,
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
					color = "$danger",
					wrap = true,
					text_size = "$font.size.sm",
				}),
				Note("Applying builds the image, which can take minutes. It keeps going if you close this sheet."),
			},
		}
	end)
end
