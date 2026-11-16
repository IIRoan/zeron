--!strict
-- Project Sandbox: run a project's agents, services and terminals in a
-- container built from its devcontainer. The engine builds and runs it
-- (crates/engine/src/sandbox.rs); this is its settings and its status.
local zeron = require("./fork_sdk")(require("./zeron")("project-sandbox"), "project-sandbox")
local Model = require("./model")(zeron)
local Settings = require("./settings")(zeron, Model)
local View = require("./view")(zeron, Model, Settings)
local PanelHome = require("@ds/panel_home")
local IconButton = require("@ds/icon_button").IconButton

local ICON = "shield-check"

-- Beside a project's other actions (Linux Workbench's project row): its
-- sandbox settings.
zeron.place {
	at = "slot:linux-workbench.project-actions",
	after = function(): Node
		local project = zeron.context("project")
		local context = Model.context(nil, if type(project) == "table" then project.id else project)
		if not context then
			return ui.div {}
		end
		local m = Model.get(context)
		local open = ui.state(`sandbox.{context.key}.actions-sheet`, false)
		return ui.row {
			items = "center",
			IconButton {
				icon = ICON,
				label = `Sandbox: {context.name}`,
				size = "sm",
				on_click = function()
					open:set(true)
				end,
			},
			Settings { model = m, open = open },
		}
	end,
}

zeron.tab("sandbox", function(tab: any): Node
	return View { session = tab.data and tab.data.session, project = tab.data and tab.data.project }
end)

-- The panel's home: open the chat's project's sandbox beside it.
zeron.place {
	at = "hook:panel.empty",
	after = function(): Node
		local session = zeron.context("session")
		local panel = zeron.context("panel")
		if not panel or not session or not session.project then
			return ui.div {}
		end
		return PanelHome.Choice {
			id = "sandbox-open",
			icon = ICON,
			label = "Sandbox",
			key = "X",
			on_click = function()
				panel.open_tab {
					kind = "sandbox",
					title = "Sandbox",
					icon = ICON,
					data = { session = session.id, project = session.project },
				}
			end,
		}
	end,
}
