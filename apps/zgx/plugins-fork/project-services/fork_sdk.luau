--!strict
-- The Linux fork's addition to the plugin SDK. plugins-sdk/zeron.luau stays
-- upstream's own; each fork plugin carries this beside its zeron.luau
-- (a byte-for-byte copy, checked by src/plugins/examples_tests.rs):
--
--   local zeron = require("./fork_sdk")(require("./zeron")("my-plugin"), "my-plugin")
--
-- zeron.workbench(method, context, params?) calls one of the fork's project
-- tools (apps/zgx/src/plugins/workbench.rs), implemented by the same engine
-- as the original desktop UI. The host derives the device, folder and
-- project from the context's ids, permits only named operations and checks
-- the plugin's grants on every call. Capture `scope` when opening a form or
-- menu so a later checkout switch refuses its stale actions.
--
--   Git                      commands("git"), sessions.read, files.read
--   Services, project sandbox,
--   project Windows VM       commands("sh"),  sessions.read, files.read
--
-- zeron.ports.parse(text) and zeron.ports.format(list) read and write port
-- lists as the engine does (zeron_proto::sandbox): numbers or `a-b` ranges,
-- separated by commas, spaces or new lines. zeron.words(text) splits such a
-- list of names.

-- The pieces of a list written with commas, spaces or new lines.
local function words(text: string): { string }
	local out = {}
	for piece in string.gmatch(text, "[^,%s]+") do
		table.insert(out, piece)
	end
	return out
end

-- Ports, or what's wrong with the first that isn't one.
local function parse(text: string): ({ number }?, string?)
	local out = {}
	for _, part in words(text) do
		local function number(p: string): number?
			local n = tonumber(p)
			return if n and n == math.floor(n) and n > 0 and n <= 65535 and string.match(p, "^%d+$") then n else nil
		end
		local a, b = string.match(part, "^(%d+)%-(%d+)$")
		if a and b then
			local from, to = number(a), number(b)
			if not from or not to then
				return nil, `{part} isn't a port or a range of ports`
			end
			if from > to then
				return nil, `{part} isn't a port range`
			end
			for port = from, to do
				table.insert(out, port)
			end
		else
			local port = number(part)
			if not port then
				return nil, `{part} isn't a port or a range of ports`
			end
			table.insert(out, port)
		end
	end
	return out, nil
end

-- Ports as the person writes them: sorted, runs folded into `a-b`.
local function format(ports: { number }?): string
	local sorted = table.clone(ports or {})
	table.sort(sorted)
	local out, from, last = {}, nil :: number?, nil :: number?
	local function close()
		if from and last then
			table.insert(out, if from == last then tostring(from) else `{from}-{last}`)
		end
	end
	for _, port in sorted do
		if last and port == last then
			continue
		elseif last and port == last + 1 then
			last = port
		else
			close()
			from, last = port, port
		end
	end
	close()
	return table.concat(out, ", ")
end

return function(zeron: any, id: string): any
	function zeron.workbench(method: string, context: any, params: any?): any
		return ui.call(`plugin.{id}.workbench`, {
			method = method,
			session = context.session or ui.null,
			project = context.project or ui.null,
			scope = context.scope or ui.null,
			params = params or {},
		})
	end
	zeron.ports = { parse = parse, format = format }
	zeron.words = words
	return zeron
end
