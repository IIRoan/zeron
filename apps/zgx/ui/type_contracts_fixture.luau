local ds = require("@ds")
ds.setup()
local ContextMenu = require("@ds/context_menu")

local busy = ui.state("type_test_busy", false)
local disabled = ui.state("type_test_disabled", false)
local actions = ui.state("type_test_actions", 0)
local callbacks = ui.state("type_test_callbacks", 0)
local styled = ui.state("type_test_styled", 0)
local selected = ui.state("type_test_selected", 0)

return ui.column {
	w_full = true,
	h_full = true,
	p = "$space.6",
	gap = "$space.4",
	ds.Button {
		"Native action",
		busy = busy,
		disabled = function(): boolean return disabled:get() end,
		on_click = ui.add(actions, 1),
	},
	ds.Button {
		"Callback",
		busy = busy,
		disabled = function(): boolean return disabled:get() end,
		on_click = function() callbacks:add(1) end,
	},
	ds.Button {
		"Styled callback",
		busy = busy,
		disabled = { opacity = 0.2 },
		on_click = function() styled:add(1) end,
	},
	ContextMenu {
		items = function()
			return {
				{ heading = ui.t("common.copy") },
				{
					label = ui.t("common.copy"),
					confirm = ui.t("common.copy"),
					icon = "copy",
					on_select = function() selected:add(1) end,
				},
			}
		end,
		ds.Button { "Menu" },
	},
}
