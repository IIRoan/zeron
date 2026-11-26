--!strict
-- The clipboard belongs to the shared VM on this device, independently
-- of a project's ports. Existing project/VM actions stay with the engine.
local zeron = require("./zeron")("windows-vm")

zeron.serve("clipboard", function(args: any): any
	if type(args) ~= "table" or type(args.enabled) ~= "boolean" then
		error("Choose whether to share the VM clipboard", 0)
	end
	local result = zeron.run("sh", {
		"-c",
		'exec "${ZERON_WINVM:-$HOME/.local/bin/winvm}" "$@"',
		"winvm",
		"clipboard",
		if args.enabled then "on" else "off",
		"--json",
	}, { timeout = 10 }):await()
	if result.status ~= 0 then
		error(if result.stderr ~= "" then result.stderr else "The VM clipboard setting could not be saved", 0)
	end
	return zeron.json.decode(result.stdout)
end)
