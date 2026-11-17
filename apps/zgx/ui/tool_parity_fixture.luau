local ds = require("@ds")
ds.setup()
local ToolDetail = require("@ds/tool_detail")

return ui.column {
	w_full = true,
	h_full = true,
	p = "$space.4",
	ToolDetail.ToolDetail {
		tool = function()
			return {
				id = "wide-diff",
				diff = {
					kind = "diff",
					path = "src/main.rs",
					lines = {
						{ kind = "hunk", text = "@@ -1 +1 @@" },
						{ kind = "-", old = 1, text = 'let old = "before";' },
						{ kind = "+", new = 1, text = 'let message = "' .. string.rep("wide code ", 30) .. 'tail_marker";' },
					},
				},
			}
		end,
	},
}
