--!strict
-- Windows VM: give a project a Windows machine (scripts/winvm, one VM on
-- this computer). The engine connects the project's ports, starts and stops
-- the VM, reads its health and runs commands in it
-- (crates/engine/src/windows_vm.rs); this is its settings and its panel.
local zeron = require("./fork_sdk")(require("./zeron")("windows-vm"), "windows-vm")
local Model = require("./model")(zeron)
local Settings = require("./settings")(zeron, Model)
local View = require("./view")(zeron, Model)
local PanelHome = require("@ds/panel_home")

local ICON = "monitor"

-- How the project uses the VM, a section of its page in Settings →
-- Projects (Linux Workbench's).
zeron.place {
	at = "slot:linux-workbench.project-environment",
	after = function(): Node
		local project = zeron.context("project")
		local context = Model.context(nil, if type(project) == "table" then project.id else project)
		if not context then
			return ui.div {}
		end
		return Settings { model = Model.get(context) }
	end,
}

zeron.tab("windows-vm", function(tab: any): Node
	return View { session = tab.data and tab.data.session, project = tab.data and tab.data.project }
end)

-- The panel's home: the chat's project's VM beside it.
zeron.place {
	at = "hook:panel.empty",
	after = function(): Node
		local session = zeron.context("session")
		local panel = zeron.context("panel")
		if not panel or not session or not session.project then
			return ui.div {}
		end
		return PanelHome.Choice {
			id = "windows-vm-open",
			icon = ICON,
			label = "Windows VM",
			key = "W",
			on_click = function()
				panel.open_tab {
					kind = "windows-vm",
					title = "Windows VM",
					icon = ICON,
					data = { session = session.id, project = session.project },
				}
			end,
		}
	end,
}
