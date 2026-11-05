--!strict
-- A session's Changes or History tab (one each: the other is another
-- tab), under a band as the browser's bar is: at its start, History's way
-- back from a commit opened in the list's place; at its end, what Changes
-- is of, the branch it's on (a click: the other branches, to switch to, or
-- a new one) and the rest under ⋯. Until the checkout's device has answered, a ring; if it can't
-- (offline, the plugin not allowed there, not a git repository), why, in
-- plain words, and a way to try again where trying can help.
--
-- The tab asks its device again every few seconds while it shows
-- (repo.luau); what changed is asked only as the work tree, the index or
-- the commit moves.

local IconButton = require("@ds/icon_button").IconButton
local git = require("./ui/git")
local Menu = require("@ds/menu")
local Glass = require("@ds/glass")
local Dropdown = require("@ds/dropdown")
local Dialog = require("@ds/dialog").Dialog
local TextField = require("@ds/text_field")
local EmptyState = require("@ds/empty_state")
local progress = require("@ds/progress")
local Branches=require("./branches")

-- What Changes shows: the work tree, the branch, the last turn.
local SCOPES = { "working", "branch", "turn" }
local SCOPE_ICON: { [string]: string } = { working = "pen", branch = "git-branch", turn = "chat-round-line" }

-- Past this width, a list stands beside what it opens (a diff, a commit);
-- under it, what's opened takes the list's place or opens under its row.
local WIDE = 760

return function(zeron: any, Repo: any, words: any)
	local ChangesView = require("./changes")(zeron, Repo, words)
	local ForkChanges = require("./fork_changes")(zeron,Repo)
	local HistoryView, CommitView = require("./history")(zeron, Repo, words)

	-- The branch the checkout is on, as a chip; a click lists the others
	-- (local first, then the remote's not here yet), and New branch.
	local function branch_switcher(m: any): Node return Branches.Button(m) end

	-- The openings already taken (view.luau's `data.view`, `data.scope`).
	local taken: { [string]: boolean } = {}

	-- What Changes is of: the work tree, the branch, the last turn (a chip
	-- of the panel's scale, the composer's footer's).
	local function scope_chip(m: any): Node
		local value = ui.use_state(m.scope:peek() :: string?)
		ui.effect(function()
			local s = m.scope:get()
			if value:peek() ~= s then
				value:set(s)
			end
		end)
		local items: { any } = {}
		for _, s in SCOPES do
			table.insert(items, { value = s, label = zeron.s(`scope.{s}`), icon = SCOPE_ICON[s], description = zeron.s(`scope.{s}_about`) })
		end
		return ui.row {
			id = "git-scope",
			shrink = 0,
			Dropdown {
				value = value,
				items = items,
				search = false,
				tone = "chip",
				icon = function(): any
					return SCOPE_ICON[m.scope:get()] or "pen"
				end,
				label = zeron.s("scope.label"),
				width = 280,
				placement = { side = "bottom", align = "start", offset = 4 },
				on_choose = function(chosen: string)
					m.set_scope(chosen, nil)
				end,
			},
		}
	end

	-- The Branch scope's base, after what Changes is of: "against" and a
	-- chip of the branches (the default, origin's own head, until another
	-- is chosen; the status's word for it while Changes has none).
	-- (`wrapped`: it stands under the scope, its words then starting where
	-- the scope chip's do.)
	local function base_chip(m: any, wrapped: State<boolean>): Node
		local value = ui.use_state(nil :: string?)
		ui.effect(function()
			local c = m.changes:get()
			local s = m.status:get()
			local now = m.base:get() or (c and c.base) or (s and s.base)
			if value:peek() ~= now then
				value:set(now)
			end
		end)
		local items = ui.computed(function(): { any }
			local out: { any } = {}
			local list = m.branches:get()
			local current = value:get()
			if not list then
				if current then
					table.insert(out, { value = current, label = current, icon = "git-branch" })
				end
				return out
			end
			for _, b in list do
				if not b.current then
					table.insert(out, { value = b.name, label = b.name, icon = if b.remote then "cloud" else "git-branch" })
				end
			end
			return out
		end)
		return ui.show(function(): boolean
			return m.scope:get() == "branch"
		end, ui.row {
			id = "git-base",
			shrink = 0,
			h = 24,
			-- Wrapped under the scope: "against" starts where the scope
			-- chip's words do (text on text, not box on box). On one line:
			-- the same gap from the chip's edge to the word as from the word
			-- to the next chip's edge.
			pl = function(): any
				if not wrapped:get() then
					return 0
				end
				-- (The chip's words start past its padding, its 12-point
				-- glyph and the gap after it.)
				local tight = (ui.theme.get("zeron.space.tight") :: number?) or 6
				return ui.raw(tight * 2 + 12, "where a chip's words start: its padding, glyph and gap")
			end,
			items = "center",
			gap = "$space.1",
			ui.text {
				zeron.t("scope.against"),
				class = "git-against",
				shrink = 0,
				-- (Lifted as a chip's words are, so all share one line.)
				mb = "$zeron.space.hair",
				text_size = "$font.size.sm",
				color = "$text.secondary",
			},
			Dropdown {
				value = value,
				items = items,
				tone = "chip",
				icon = "git-branch",
				placeholder = zeron.t("scope.base_choose"),
				search = zeron.t("branches.search"),
				label = zeron.s("scope.base_label"),
				width = 280,
				placement = { side = "bottom", align = "start", offset = 4 },
				on_open = function()
					m.load_branches()
				end,
				on_choose = function(chosen: string)
					m.set_scope("branch", chosen)
				end,
			},
		})
	end

	-- The rest of what can be done to the checkout, under ⋯: fetch, pull,
	-- discard everything (Changes asks first).
	local GroupedMenu=require("./grouped_menu")
	local function more_menu(m: any): Node return GroupedMenu(m) end

	-- (`panel`: the chat's panel, read as its tab was built: what opens a
	-- commit in a tab of its own.)
	return ui.component(function(props: { tab: any, view: string, panel: any? }): Node
		local data = props.tab.data or {}
		local here = zeron.context("session")
		local id = data.session or (here and here.id)
		if type(id) ~= "string" or id == "" then
			return EmptyState { icon = "git-branch", title = zeron.s("tab.no_session_title"), body = zeron.s("tab.no_session_body"), fill = true }
		end
		local m = Repo.get(id)
		-- (Asked again as its device says it changed, while the tab is on
		-- screen: `on_visible` below.)
		local shows = ui.use_state(false)
		m.watch(shows)
		-- What it shows: its tab's own, Changes or History, for good (another
		-- is another tab).
		local view = props.view
		-- What Changes was opened to show (`data.scope`: "working", "branch",
		-- "turn"), taken once per opening: coming back to the tab later
		-- keeps what was chosen in it since.
		local asked = `{id}|{data.opened or ""}|{data.scope or ""}`
		if not taken[asked] then
			taken[asked] = true
			if data.scope == "working" or data.scope == "branch" or data.scope == "turn" then
				m.set_scope(data.scope, nil)
			end
		end
		-- Which layout its width is for: 0 until measured, then "wide" or
		-- "narrow". (Only a crossing of WIDE builds the page again: the text
		-- reflows live as an edge is dragged.)
		local width = ui.use_state("")
		local scope_wrapped = ui.use_state(false)

		-- (A band as the panel's others are: the browser's bar, the file's
		-- head; 40 points, its controls the panel's 24, its first under the
		-- strip's first tab, its last under the strip's end.)
		local head = ui.row {
			id = "git-head",
			w_full = true,
			-- (40 points, as the panel's other bands; taller only when what
			-- Changes is of and its base must take two lines.)
			min_h = 40,
			py = "$space.2",
			-- (Its last control ends where the card's under it do: the
			-- card's edge and its head's trailing inset.)
			pr = ui.raw(git.end_inset(), "the card's edge and its head's trailing inset"),
			shrink = 0,
			gap = "$space.1",
			-- (Its ends on the first line: the base, wrapped, under it.)
			items = "start",
			-- At the band's start, what Changes is of and, at the Branch
			-- scope, what it's against, on one line; the base goes under it
			-- only when the band has no room for both.
			if view == "changes"
				then ui.row {
					id = "git-scope-group",
					grow = 1,
					min_w = 0,
					flex_wrap = true,
					gap = "$space.1",
					items = "center",
					-- (Taller than a chip: the base has wrapped under it.)
					on_resize = function(size)
						local now = size.h > 30
						if scope_wrapped:peek() ~= now then
							scope_wrapped:set(now)
						end
					end,
					scope_chip(m),
					base_chip(m, scope_wrapped),
				}
				else ui.div { grow = 1 },
			-- At its end, as the browser's page controls are: the branch,
			-- the rest.
			ui.row {
				id = "git-head-controls",
				shrink = 0,
				gap = "$space.1",
				items = "center",
				branch_switcher(m),
				more_menu(m),
			},
		}

		-- What the tab shows: asking, why it can't, or the view chosen.
		local function phase(): string
			local p = m.problem:get()
			if p and not m.status:get() then
				return `problem:{p.reason}:{p.code or ""}`
			end
			-- (Laid out once its width is known: the layout is built for it.)
			if not m.status:get() or width:get() == "" then
				return "asking"
			end
			return `ready:{view}:{width:get()}:{m.scope:get()}`
		end
		local content = ui.presence(phase, function(key: string): Node
			if key == "asking" then
				return ui.column {
					w_full = true,
					grow = 1,
					items = "center",
					justify = "center",
					gap = "$space.3",
					progress.ProgressRing { size = 20, tone = "neutral", label = zeron.s("loading") },
					ui.text { zeron.t("tab.asking"), text_size = "$font.size.sm", color = "$text.tertiary" },
				}
			end
			local problem = string.match(key, "^problem:")
			if problem then
				local p = m.problem:peek()
				local said = words(p, m.status:peek())
				return EmptyState {
					icon = if p.code == "not_a_repo" or p.code == "no_checkout"
						then "folder"
						elseif p.reason == "offline" or p.reason == "timeout" then "wifi-off"
						else "plug-circle",
					title = said.title,
					body = said.body,
					action = if said.retry
						then {
							label = zeron.s("try_again"),
							on_click = function()
								m.refresh(true)
							end,
						}
						else nil,
					fill = true,
				}
			end
			local which, layout = string.match(key, "^ready:([^:]+):([^:]+)")
			local wide = layout == "wide"
			if which == "history" then
				return HistoryView { model = m, wide = wide, panel = props.panel }
			elseif which == "commit" then
				-- (A commit's own tab: what it was opened with.)
				local c = data.commit
				if type(c) ~= "table" or type(c.sha) ~= "string" then
					return EmptyState { icon = "git-commit", title = zeron.s("history.pick_title"), body = zeron.s("history.pick_body"), size = "sm", fill = true }
				end
				return CommitView { model = m, c = c, wide = wide, panel = props.panel }
			end
			if m.scope:peek()=="working" then return ForkChanges {model=m,wide=wide,panel=props.panel} end
			return ChangesView { model = m, wide = wide }
		end, { w_full = true, grow = 1, min_h = 0 })

		return ui.column {
			id = "git-tab",
			Branches.View {model=m},
			Dialog {open=m.workbench.commit_ask,title="Stage all changes and commit?",body="There are no staged changes. This will stage the current working changes in this repository before committing.",confirm={label="Stage all and commit",on_click=m.workbench.confirm_commit}},
			class = "git-tab",
			-- Also keep menus solid when the layout plugin is disabled.
			theme = { ["zeron.menu.tint"] = "alpha({zeron.menu.solid}, 1)", ["zeron.menu.blur"] = 0,
				["menu.bg"] = "{zeron.menu.tint}" },
			w_full = true,
			h_full = true,
			on_visible = function(now: boolean)
				if shows:peek() ~= now then
					shows:set(now)
				end
			end,
			-- (Its sides the tab strip's and the browser bar's: the first
			-- control under the first tab, the last under the strip's end.)
			px = "$space.2",
			-- (A commit's tab, with no band: room over its title instead.)
			pt = if view == "commit" then "$space.3" else nil,
			pb = "$space.2",
			gap = "$space.2",
			on_resize = function(size)
				-- (No width is no width: a closing panel lays out at 0.)
				if size.w <= 0 then
					return
				end
				local now = if size.w >= WIDE then "wide" else "narrow"
				if width:peek() ~= now then
					width:set(now)
				end
			end,
			-- (The head once there's a branch to say: until then, nothing
			-- it would say is known.)
			-- (A commit's tab has none: it's the commit's, not the checkout's.)
			ui.show(function(): boolean
				return view ~= "commit" and m.status:get() ~= nil
			end, head),
			content,
		}
	end)
end
