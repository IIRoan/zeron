--!strict
-- Windows VM: give a project a Windows machine (scripts/winvm, one VM on
-- this computer). The engine connects the project's ports, starts and stops
-- the VM, reads its health and runs commands in it
-- (crates/engine/src/windows_vm.rs); this is its settings and its panel.
local zeron = require("./fork_sdk")(require("./zeron")("windows-vm"), "windows-vm")
local Model = require("./model")(zeron)
local Settings = require("./settings")(zeron, Model)
local View = require("./view")(zeron, Model, Settings)
local PanelHome = require("@ds/panel_home")
local IconButton = require("@ds/icon_button").IconButton

local ICON = "monitor"

-- Beside a project's other actions (Linux Workbench's project row): how the
-- project uses the VM.
zeron.place {
	at = "slot:linux-workbench.project-actions",
	after = function(): Node
		local project = zeron.context("project")
		local context = Model.context(nil, if type(project) == "table" then project.id else project)
		if not context then
			return ui.div {}
		end
		local m = Model.get(context)
		local open = ui.state(`windows-vm.{context.key}.actions-sheet`, false)
		return ui.row {
			items = "center",
			IconButton {
				icon = ICON,
				label = `Windows VM: {context.name}`,
				size = "sm",
				on_click = function()
					open:set(true)
				end,
			},
			Settings.Sheet { model = m, open = open },
		}
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
