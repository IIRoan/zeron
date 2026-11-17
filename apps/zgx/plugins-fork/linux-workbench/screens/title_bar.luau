-- The title bar where the original Linux fork had it: upstream's own
-- (`@app-original`), its box over the chat list at the right
-- (fork_layout.bar_w), so the sidebar's toggle, Back, Forward and the
-- list's "…" stand at the list's top left and the window's buttons at the
-- window's top right.
local original = require("@app-original/screens/shell/title_bar")
local fork_layout = require("./fork_layout")

return function(props: any): Node
	return ui.div {
		absolute = true,
		top = 0,
		right = 0,
		w = fork_layout.bar_w,
		pointer_events = "none",
		original(props),
	}
end
