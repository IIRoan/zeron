--!strict
-- (Fork) Git history is off in the Linux fork: no History tab (its commit
-- graph) is offered or opened, and one left open from before says so.
-- Upstream's History stays as upstream has it (history.luau, init.luau's
-- row, key and tabs), so its changes merge as they come: this file and a
-- guard in view.luau turn it off. `on = true` brings it back.
local M = { on = false }

-- Once, as the view is made: with the plugin's SDK and its repository
-- model (`Repo.open_tab` is looked up as it's called, so wrapping it here
-- reaches init.luau's callers too).
function M.apply(zeron: any, Repo: any)
	if M.on then
		return
	end
	-- Nothing opens it: the panel home's row and its H key, or any caller.
	local open = Repo.open_tab
	Repo.open_tab = function(panel: any, session: string, view: string, scope: string?)
		if view == "history" then
			return
		end
		return open(panel, session, view, scope)
	end
	-- The panel home's History row, out of its list: init.luau builds it
	-- with the design system's row (`PanelHome.Choice`, looked up as each
	-- row is built), so that row alone isn't drawn. (A `ui.change` replace
	-- of `#git-open-history` doesn't reach a placement's own rows.)
	local PanelHome = require("@ds/panel_home")
	local choice = PanelHome.Choice
	PanelHome.Choice = function(props: any): Node
		if props.id == "git-open-history" then
			return ui.div { hidden = true }
		end
		return choice(props)
	end
end

return M
