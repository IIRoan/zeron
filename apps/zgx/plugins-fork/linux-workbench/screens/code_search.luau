-- (Linux Workbench) The Explorer's Code view, as the desktop app's
-- (crates/ui/src/files/repo_search.rs): the search field; Files | Code,
-- then Aa (case), ab (whole word), .* (regular expression) and Filters
-- (a folder, files to include and exclude, ignored files); a line saying
-- what was found, with refresh, collapse all, expand all, copy and clear;
-- then the matches under their files, each line's number and its text
-- with the match lit. A match opens its file at that line.
local route = require("@app/screens/shell/route")
local Icon = require("@ds/icon")
local IconButton = require("@ds/icon_button").IconButton
local FileIcon = require("@ds/file_icons")
local S = require("./files_search")
local fm = require("../ui/fork_motion")

local ROW = 24

-- A small toggle of the toolbar, lit while on.
local function toggle(text: string, name: string, on: () -> boolean, flip: () -> (), mono: boolean?): Node
	return ui.button {
		label = name,
		tooltip = name,
		selected = on,
		h = 22,
		px = "$space.2",
		rounded = "$radius.sm",
		bg = function(): string
			return if on() then "$zeron.secondary.hover" else "#00000000"
		end,
		hover = { bg = "$hover" },
		on_click = flip,
		ui.text {
			text,
			font = if mono then "$font.mono" else nil,
			text_size = "$font.size.xs",
			color = function(): string
				return if on() then "$text.primary" else "$text.secondary"
			end,
		},
	}
end

-- A match's line with what matched lit (`ranges`: 0-based bytes).
local function preview(hit: any): Node
	local text = tostring(hit.text or "")
	local spans: { [any]: any } = {
		literal = true,
		font = "$font.mono",
		text_size = "$font.size.xs",
		color = "$text.secondary",
	}
	local at = 1
	for _, r in hit.ranges or {} do
		local from, to = (tonumber(r.from) or 0) + 1, tonumber(r.to) or 0
		if from > at then
			table.insert(spans, ui.span { string.sub(text, at, from - 1), literal = true })
		end
		if to >= from then
			table.insert(spans, ui.span { string.sub(text, from, to), literal = true, bg = "$selection", color = "$text.primary" })
		end
		at = math.max(at, to + 1)
	end
	if at <= #text then
		table.insert(spans, ui.span { string.sub(text, at), literal = true })
	end
	return ui.rich(spans)
end

-- (`head`, `lead`: the pane's band, its height and the room its tree
-- toggle keeps at its start, so the field stands where the tree's filter does.)
return ui.component(function(props: { head: number?, lead: number? }): Node
	local delayed = ui.debounced(S.query, 180)
	local again = ui.use_state(0)
	local collapsed = ui.use_state({} :: { [string]: boolean })
	local field = ui.ref()
	ui.effect(function()
		S.focus:get()
		ui.after(0, function()
			local el = field:peek()
			if el then
				el:focus()
			end
		end)
	end)
	local result = ui.resource(function(): any
		again:get()
		local c = route.session:get()
		local q = delayed:get()
		if not c or q == "" then
			return nil
		end
		return ui.call("files.search_content", {
			target = { chat = c.id, space = c.project, device = c.device, root = c.cwd },
			query = q,
			limit = 300,
			per_file = 20,
			options = {
				matchCase = S.match_case:get(),
				wholeWord = S.whole:get(),
				useRegex = S.regex:get(),
				include = S.include:get(),
				exclude = S.exclude:get(),
				folder = S.folder:get(),
				includeIgnored = S.ignored:get(),
			},
		}):await()
	end)
	-- What the last search failed with (a regular expression that doesn't
	-- parse, say), as the desktop app's: the results gone, the whole message
	-- in their place. (The last answer's matches are kept as a search fails:
	-- they were listed under the error.)
	local function failure(): string?
		local e = result.error:get()
		if S.query:get() == "" or not e or tostring(e) == "superseded" then
			return nil
		end
		return (string.gsub(tostring(e), "^bad params: ", ""))
	end
	local function matches(): { any }
		local r = if S.query:get() ~= "" and not failure() then result.data:get() else nil
		return (r and r.matches) or {}
	end
	-- Files in the order their matches came, each then its lines.
	local rows = ui.computed(function(): { any }
		local by: { [string]: { any } } = {}
		local order: { string } = {}
		for _, m in matches() do
			if not by[m.path] then
				by[m.path] = {}
				table.insert(order, m.path)
			end
			table.insert(by[m.path], m)
		end
		local shut = collapsed:get()
		local out = {}
		for _, path in order do
			table.insert(out, { key = `f:{path}`, path = path, count = #by[path] })
			if not shut[path] then
				for _, m in by[path] do
					table.insert(out, { key = `m:{path}:{m.line}:{m.column}`, hit = m })
				end
			end
		end
		return out
	end)
	local function status(): string
		if not route.session:get() then
			return "Open a chat to search its checkout."
		end
		if S.query:get() == "" then
			return "Search the checkout's code."
		end
		local r = result.data:get()
		if result.loading:get() and not r then
			return "Searching…"
		end
		if failure() then
			return "Search failed"
		end
		if not r then
			return "Searching…"
		end
		if r.indexing then
			return "Indexing workspace…"
		end
		local lines = #(r.matches or {})
		local seen, files = {}, 0
		for _, m in r.matches or {} do
			if not seen[m.path] then
				seen[m.path] = true
				files += 1
			end
		end
		return `{lines} matching line{if lines == 1 then "" else "s"} in {files} file{if files == 1 then "" else "s"}{if r.truncated then " · Results limited; narrow your search" else ""}`
	end
	local function all_collapsed(shut: boolean)
		local next = {}
		if shut then
			for _, m in matches() do
				next[m.path] = true
			end
		end
		collapsed:set(next)
	end
	local function copy()
		local out = {}
		for _, m in matches() do
			table.insert(out, `{m.path}:{m.line}: {m.text}`)
		end
		if #out > 0 then
			ui.clipboard.write(table.concat(out, "\n"))
		end
	end
	local function open(hit: any)
		local c = route.session:peek()
		if c then
			require("@app/screens/panel").open_file(c, hit.path, nil, { line = hit.line, column = hit.column })
		end
	end
	local function filter_field(label: string, value: any, placeholder: string): Node
		return ui.column {
			w_full = true,
			gap = ui.raw(3, "a filter's label over its field"),
			ui.text { label, text_size = "$font.size.xs", color = "$text.tertiary" },
			ui.input {
				value = value,
				label = label,
				placeholder = placeholder,
				w_full = true,
				h = 26,
				px = "$space.2",
				rounded = "$radius.sm",
				bg = "$zeron.secondary.bg",
				border = 0,
				font = "$font.mono",
				text_size = "$font.size.sm",
			},
		}
	end
	local filtered = function(): boolean
		return S.filters_open:get() or S.folder:get() ~= "" or S.include:get() ~= "" or S.exclude:get() ~= "" or S.ignored:get()
	end

	return ui.column {
		id = "fork-code-search",
		w_full = true,
		h_full = true,
		min_h = 0,
		-- The field, where the tree's filter is.
		ui.row {
			w_full = true,
			shrink = 0,
			h = ui.raw(props.head or 40, "the pane's band"),
			pl = ui.raw(props.lead or 8, "past the pane's tree toggle"),
			pr = "$space.2",
			items = "center",
			ui.row {
				w_full = true,
				h = 32,
				px = "$space.2",
				gap = "$space.2",
				items = "center",
				rounded = "$radius.md",
				bg = "$zeron.secondary.bg",
				color = "$text.tertiary",
				Icon { name = "search", size = 14 },
				ui.input {
					ref = field,
					value = S.query,
					label = "Search code",
					placeholder = "Search code",
					grow = 1,
					h = 28,
					px = 0,
					py = 0,
					bg = "#00000000",
					border = 0,
					text_size = "$font.size.md",
					focus = { border = 0 },
					on_key = function(e: any): boolean?
						if e.key == "Escape" then
							if S.query:peek() ~= "" then
								S.query:set("")
							else
								S.mode:set("files")
							end
							return true
						end
						return false
					end,
				},
			},
		},
		-- Files | Code, then the code search's own toggles.
		ui.row {
			w_full = true,
			shrink = 0,
			px = "$space.2",
			gap = ui.raw(2, "the toolbar's buttons, close"),
			items = "center",
			toggle("Files", "Search files", function(): boolean
				return false
			end, function()
				S.mode:set("files")
			end),
			toggle("Code", "Search code", function(): boolean
				return true
			end, function() end),
			ui.div { grow = 1 },
			toggle("Aa", "Match case", function(): boolean
				return S.match_case:get()
			end, function()
				S.match_case:set(not S.match_case:peek())
			end, true),
			toggle("ab", "Match whole word", function(): boolean
				return S.whole:get()
			end, function()
				S.whole:set(not S.whole:peek())
			end, true),
			toggle(".*", "Use regular expression", function(): boolean
				return S.regex:get()
			end, function()
				S.regex:set(not S.regex:peek())
			end, true),
			toggle("Filters", "Search details", filtered, function()
				S.filters_open:set(not S.filters_open:peek())
			end),
		},
		fm.show(function(): boolean
			return S.filters_open:get()
		end, ui.column {
			id = "fork-code-search-filters",
			w_full = true,
			shrink = 0,
			px = "$space.3",
			pt = "$space.2",
			gap = "$space.2",
			filter_field("Search in folder", S.folder, "Project root"),
			filter_field("Files to include", S.include, "e.g. src/**, *.rs"),
			filter_field("Files to exclude", S.exclude, "e.g. tests/**"),
			ui.text { "Comma-separated globs · ./ anchors to workspace", text_size = "$font.size.xs", color = "$text.tertiary" },
			toggle("Include ignored files", "Include files excluded by ignore rules", function(): boolean
				return S.ignored:get()
			end, function()
				S.ignored:set(not S.ignored:peek())
			end),
		}),
		-- What was found, and what to do with it.
		ui.row {
			w_full = true,
			shrink = 0,
			min_h = 26,
			pl = "$space.3",
			pr = "$space.2",
			gap = "$space.1",
			items = "center",
			ui.text { status, grow = 1, min_w = 0, text_size = "$font.size.xs", color = "$text.tertiary", truncate = true },
			fm.fade(function(): boolean
				return #matches() > 0
			end, ui.row {
				shrink = 0,
				items = "center",
				IconButton { icon = "refresh", label = "Refresh search", size = "sm", tone = "quiet", on_click = function()
					again:set(again:peek() + 1)
				end },
				IconButton { icon = "alt-arrow-up", label = "Collapse all results", size = "sm", tone = "quiet", on_click = function()
					all_collapsed(true)
				end },
				IconButton { icon = "alt-arrow-down", label = "Expand all results", size = "sm", tone = "quiet", on_click = function()
					all_collapsed(false)
				end },
				IconButton { icon = "copy", label = "Copy search results", size = "sm", tone = "quiet", on_click = copy },
				IconButton { icon = "close", label = "Clear search results", size = "sm", tone = "quiet", on_click = function()
					S.query:set("")
				end },
			}),
		},
		-- The matches, under their files.
		ui.scroll {
			id = "fork-code-search-results",
			w_full = true,
			grow = 1,
			min_h = 0,
			ui.column {
				w_full = true,
				ui.show(function(): boolean
					return failure() ~= nil
				end, ui.text {
					function(): string
						return failure() or ""
					end,
					id = "fork-code-search-error",
					literal = true,
					wrap = true,
					w_full = true,
					px = "$space.3",
					py = "$space.2",
					font = "$font.mono",
					text_size = "$font.size.xs",
					color = "$danger",
				}),
				-- (Files folding and new results: the rest slide into place.)
				fm.each(rows, function(row: any): string
					return row.key
				end, function(row: any): Node
					if row.path then
						local path = row.path
						local name = string.match(path, "([^/]+)$") or path
						return ui.button {
							class = "code-search-file",
							label = path,
							expanded = function(): boolean
								return not collapsed:get()[path]
							end,
							w_full = true,
							h = ROW,
							px = "$space.3",
							gap = ui.raw(5, "the row's chevron, icon and words"),
							items = "center",
							justify = "start",
							hover = { bg = "$hover" },
							on_click = function()
								local next = table.clone(collapsed:peek())
								next[path] = if next[path] then nil else true
								collapsed:set(next)
							end,
							Icon {
								name = function(): string
									return if collapsed:get()[path] then "alt-arrow-right" else "alt-arrow-down"
								end,
								size = 10,
								color = "$text.tertiary",
							},
							FileIcon { name = name, size = 14 },
							ui.text { path, grow = 1, min_w = 0, truncate = true, text_size = "$font.size.sm", color = "$text.primary" },
							ui.text { tostring(row.count), shrink = 0, text_size = "$font.size.xs", color = "$text.tertiary" },
						}
					end
					local hit = row.hit
					return ui.button {
						class = "code-search-match",
						label = `{hit.path}:{hit.line} {hit.text or ""}`,
						w_full = true,
						h = ROW,
						px = "$space.3",
						gap = ui.raw(5, "the line's number and its text"),
						items = "center",
						justify = "start",
						hover = { bg = "$hover" },
						on_click = function()
							open(hit)
						end,
						ui.text { tostring(hit.line), w = 36, shrink = 0, align = "right", font = "$font.mono", text_size = "$font.size.xs", color = "$text.tertiary" },
						ui.row { grow = 1, min_w = 0, overflow = "hidden", preview(hit) },
					}
				end),
			},
		},
	}
end)
