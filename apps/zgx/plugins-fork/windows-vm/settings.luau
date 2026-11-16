--!strict
-- How a project uses the Windows VM, a section of its page in Settings →
-- Projects: which of its ports reach the VM and back, which of its
-- services start it, whether its viewer opens. The VM itself is one
-- machine (scripts/winvm); a project connects to it.
local Button = require("@ds/button")
local TextField = require("@ds/text_field")
local Switch = require("@ds/switch")
local Settings = require("@ds/settings")
local Spinner = require("@ds/status").Spinner

local BUSY = {
	save = "Saving…",
	disconnect = "Disconnecting…",
	start = "Connecting this project's ports (starting the VM first if it is stopped)…",
	stop = "Shutting Windows down…",
	view = "Opening the viewer…",
}

return function(zeron: any, Model: any)
	-- What the VM is to this project now, in a line.
	local function status(m: any): string
		local r = m.reply:get()
		if not r then
			return "Reading the project's Windows VM…"
		end
		if not r.settings then
			return if r.available
				then "Off · this project's agents have no Windows VM."
				else "The Windows VM isn't set up on this machine yet (scripts/winvm)."
		end
		local vm = r.vm or {}
		local line
		if vm.state == "running" then
			local ports = zeron.ports.format(vm.toVm)
			line = if ports == ""
				then "Connected · the VM is running. No port is connected yet."
				else `Connected · the VM is running. Ports connected: {ports}.`
		elseif vm.state == "starting" then
			line = "Connected · the VM is starting…"
		elseif vm.state == "stopped" then
			line = "Connected · the VM is stopped. Start it here, or an agent starts it when it needs it."
		else
			line = "Connected · the VM can't be used on this machine yet."
		end
		for _, extra in { vm.error, vm.tunnelError } do
			if extra and extra ~= "" then
				line ..= ` {extra}`
			end
		end
		return line
	end

	return ui.component(function(p: any): Node
		local m = p.model
		local auto_ports = ui.use_state(true)
		local to_vm = ui.use_state("")
		local from_vm = ui.use_state("")
		local start_with = ui.use_state("")
		local viewer = ui.use_state(true)
		local problem = ui.use_state("")
		-- Filled once the reply comes (and after a save answers), never
		-- under the person's typing.
		local filled = false

		local function fill(settings: any?)
			auto_ports:set(not settings or settings.autoPorts ~= false)
			to_vm:set(zeron.ports.format(settings and settings.toVm))
			from_vm:set(zeron.ports.format(settings and settings.fromVm))
			start_with:set(table.concat((settings and settings.startWith) or {}, ", "))
			viewer:set(not settings or settings.viewer ~= false)
		end

		ui.after(0, function()
			m.error:set("")
			m.refresh()
		end)
		ui.effect(function()
			local r = m.reply:get()
			if r and not filled then
				filled = true
				fill(r.settings)
			end
		end)
		ui.every(5000, m.refresh)

		local function save()
			problem:set("")
			local into, into_error = zeron.ports.parse(to_vm:peek())
			if not into then
				problem:set(`More ports into the VM: {into_error}`)
				return
			end
			local out, out_error = zeron.ports.parse(from_vm:peek())
			if not out then
				problem:set(`Ports from the VM: {out_error}`)
				return
			end
			m.save({
				autoPorts = auto_ports:peek(),
				toVm = into,
				fromVm = out,
				startWith = zeron.words(start_with:peek()),
				viewer = viewer:peek(),
			}, function()
				filled = false
			end)
		end

		local function running(): boolean
			local s = m.state()
			return s == "running" or s == "starting"
		end
		local function busy(): boolean
			return m.busy:get() ~= ""
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

		return Settings.Section {
			title = "Windows VM",
			description = "Give this project a Windows machine. Agents use it through their tools (vm_exec, vm_screenshot, …), starting it when they need it.",
			ui.column {
				id = "windows-vm-settings",
				w_full = true,
				gap = "$space.3",
				ui.text {
					function()
						return status(m)
					end,
					px = Settings.inset(),
					wrap = true,
					color = "$text.secondary",
					text_size = "$font.size.sm",
				},
				Settings.Group {
					switch(
						"Connect this project's servers automatically",
						auto_ports,
						"A server this project starts (in any worktree, or in its sandbox) reaches Windows on its own port."
					),
					field("More ports into the VM", to_vm, "3000, 3001", "Ports of this machine Windows reaches as its own localhost."),
					field("Ports from the VM", from_vm, "None", "Ports of a server in Windows this machine reaches."),
					field("Start with services", start_with, "None", "Services of this project that start the VM when they start (their names)."),
					switch("Open the viewer when the VM starts", viewer, "Windows' screen in a window of its own on this machine."),
				},
				ui.show(busy, ui.row {
					px = Settings.inset(),
					gap = "$space.2",
					items = "center",
					Spinner { size = 14 },
					ui.text {
						function()
							return (BUSY :: any)[m.busy:get()] or "Working…"
						end,
						wrap = true,
						text_size = "$font.size.sm",
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
					flex_wrap = true,
					ui.show(m.connected, Button { "Disconnect", tone = "ghost", size = "sm", disabled = busy, on_click = m.disconnect }),
					ui.div { grow = 1 },
					ui.show(function()
						return m.connected() and running()
					end, ui.row {
						gap = "$space.2",
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
							"Open viewer",
							tone = "secondary",
							size = "sm",
							disabled = busy,
							on_click = function()
								m.control("view")
							end,
						},
						Button {
							"Stop VM",
							tone = "secondary",
							size = "sm",
							disabled = busy,
							on_click = function()
								m.control("stop")
							end,
						},
					}),
					ui.show(function()
						return m.connected() and not running()
					end, Button {
						"Start VM",
						tone = "secondary",
						size = "sm",
						disabled = busy,
						on_click = function()
							m.control("start")
						end,
					}),
					Button {
						function()
							return if m.connected() then "Apply" else "Connect"
						end,
						size = "sm",
						busy = function()
							return m.busy:get() == "save"
						end,
						disabled = function()
							local r = m.reply:get()
							return busy() or r == nil or (not r.available and not r.settings)
						end,
						on_click = save,
					},
				},
			},
		}
	end)
end
