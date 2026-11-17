--!strict
-- Project Sandbox: run a project's agents, services and terminals in a
-- container built from its devcontainer. The engine builds and runs it
-- (crates/engine/src/sandbox.rs); this is its settings and its status.
local zeron = require("./fork_sdk")(require("./zeron")("project-sandbox"), "project-sandbox")
local Model = require("./model")(zeron)
local Settings = require("./settings")(zeron, Model)
local View = require("./view")(zeron, Model)
local PanelHome = require("@ds/panel_home")

local ICON = "shield-check"

-- Its settings, a section of the project's page in Settings → Projects
-- (Linux Workbench's).
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

zeron.tab("sandbox", function(tab: any): Node
	return View {
		session = tab.data and tab.data.session,
		project = tab.data and tab.data.project,
		-- (Its chat's panel, read as the tab builds: Open terminal asks it.)
		panel = zeron.context("panel"),
	}
end)

-- Open terminal's ask where Linux Workbench isn't there to take it (the
-- stock panel opens it as a tab of ours): where the chat's terminal is.
zeron.tab("shell-terminal", function(): Node
	return ui.column {
		w_full = true,
		p = "$space.4",
		ui.text { "Ctrl+J opens this chat's terminal.", color = "$text.tertiary", text_size = "$font.size.sm" },
	}
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
