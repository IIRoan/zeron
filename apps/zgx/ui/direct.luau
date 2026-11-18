local ds = require("@ds")
ds.setup()

local address = ui.shared("direct.address", "")
local key = ui.state("pairing_key", "")
local pairing = ui.state("pairing", false)
local problem = ui.state("pairing_error", nil :: string?)

local function pair()
	if pairing:peek() or #key:peek() == 0 then
		return
	end
	pairing:set(true)
	problem:set(nil)
	ui.call("direct.pair", key:peek()):after(function()
		key:set("")
	end, function(err: any)
		pairing:set(false)
		problem:set(err.message)
	end)
end

return ui.column {
	w_full = true,
	h_full = true,
	bg = "$background",
	ds.TitleBar { collapsed = ui.state("pairing_title_bar", false), lights = true },
	ui.column {
		w_full = true,
		grow = 1,
		items = "center",
		justify = "center",
		px = "$space.6",
		ui.column {
			w_full = true,
			max_w = 360,
			gap = "$space.4",
			ds.Text { "Connect to My Instance", style = "title", heading = 1 },
			ds.Text { function() return address:get() end, tone = "secondary" },
			ds.TextField {
				value = key,
				label = "One-time pairing key",
				password = true,
				w_full = true,
				autofocus = true,
				disabled = pairing,
				error = function() return problem:get() end,
				hint = "Run zeron-zgx-pair on Linux.",
				on_commit = pair,
			},
			ui.row {
				justify = "end",
				w_full = true,
				ds.Button {
					function() return if pairing:get() then "Pairing…" else "Pair with Linux" end,
					disabled = function() return pairing:get() or #key:get() == 0 end,
					on_click = pair,
				},
			},
		},
	},
}
