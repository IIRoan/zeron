--!strict
-- How a project uses the Windows VM: which of its ports reach the VM and
-- back, which of its services start it, whether its viewer opens. The VM
-- itself is one machine (scripts/winvm); a project connects to it.
local Button = require("@ds/button")
local TextField = require("@ds/text_field")
local Switch = require("@ds/switch")
local Sheet = require("@ds/dialog").Sheet
local Spinner = require("@ds/status").Spinner

local BUSY = {
	save = "Saving…",
	disconnect = "Disconnecting…",
	start = "Connecting this project's ports (starting the VM first if it is stopped)…",
	stop = "Shutting Windows down…",
	view = "Opening the viewer…",
}

return function(zeron: any, Model: any)
	local function Note(text: string): Node
		return ui.text { text, wrap = true, color = "$text.tertiary", text_size = "$font.size.xs" }
	end

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

	local Sheet_ = ui.component(function(p: any): Node
		local m = p.model
		local open = p.open or m.settings
		local auto_ports = ui.use_state(true)
		local to_vm = ui.use_state("")
		local from_vm = ui.use_state("")
		local start_with = ui.use_state("")
		local viewer = ui.use_state(true)
		local problem = ui.use_state("")
		local filled = false

		local function fill(settings: any?)
			auto_ports:set(not settings or settings.autoPorts ~= false)
			to_vm:set(zeron.ports.format(settings and settings.toVm))
			from_vm:set(zeron.ports.format(settings and settings.fromVm))
			start_with:set(table.concat((settings and settings.startWith) or {}, ", "))
			viewer:set(not settings or settings.viewer ~= false)
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
			local r = m.reply:get()
			if open:get() and r and not filled then
				filled = true
				fill(r.settings)
			end
		end)
		ui.every(5000, function()
			if open:peek() then
				m.refresh()
			end
		end)

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
			return ui.column {
				gap = "$space.1",
				w_full = true,
				TextField { value = value, label = label, placeholder = placeholder, w_full = true, font = "$font.mono" },
				Note(hint),
			}
		end
		local function switch(label: string, value: any, hint: string): Node
			return ui.row {
				w_full = true,
				items = "center",
				gap = "$space.3",
				ui.column { grow = 1, gap = 2, ui.text { label, text_size = "$font.size.sm", wrap = true }, Note(hint) },
				Switch { value = value, label = label },
			}
		end

		return Sheet {
			open = open,
			size = 560,
			title = function()
				return `Windows VM · {m.context.name}`
			end,
			foot = ui.row {
				w_full = true,
				gap = "$space.2",
				items = "center",
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
			ui.column {
				w_full = true,
				gap = "$space.4",
				ui.column {
					gap = "$space.1",
					ui.text { "Give this project a Windows machine", weight = "$font.weight.semibold" },
					ui.text {
						function()
							return status(m)
						end,
						wrap = true,
						color = "$text.secondary",
						text_size = "$font.size.sm",
					},
				},
				switch(
					"Connect the ports of this project's servers automatically",
					auto_ports,
					"A server this project starts (in any worktree, or in its sandbox) reaches Windows on its own port."
				),
				field("More ports into the VM", to_vm, "3000, 3001", "Ports of this machine Windows reaches as its own localhost."),
				field("Ports from the VM", from_vm, "None", "Ports of a server in Windows this machine reaches."),
				field("Start with services", start_with, "None", "Services of this project that start the VM when they start (their names)."),
				switch("Open the viewer window when the VM starts", viewer, "Windows' screen in a window of its own on this machine."),
				ui.show(busy, ui.row {
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
					color = "$danger",
					wrap = true,
					text_size = "$font.size.sm",
				}),
				Note("Agents in this project use the VM through their tools (vm_exec, vm_screenshot, …), starting it when they need it."),
			},
		}
	end)

	return { Sheet = Sheet_, status = status }
end
