-- Workspace search, as the desktop app's Explorer has it
-- (crates/ui/src/files/repo_search.rs): the Explorer's Code view
-- (./code_search) in the tree's place, its query and options kept here so
-- they last as the pane is made again. ⇧⌘F, the rail's search, the tree's
-- search button and a folder's "Find in Folder…" open it.
local M = {
	-- What the Explorer shows: its files ("files") or code matches ("code").
	mode = ui.state("fork.explorer.mode", "files"),
	query = ui.state("fork.search.query", ""),
	folder = ui.state("fork.files.search_folder", ""),
	include = ui.state("fork.search.include", ""),
	exclude = ui.state("fork.search.exclude", ""),
	match_case = ui.state("fork.search.match_case", false),
	whole = ui.state("fork.search.whole_word", false),
	regex = ui.state("fork.search.regex", false),
	ignored = ui.state("fork.search.ignored", false),
	filters_open = ui.state("fork.search.filters_open", false),
	-- Moved to put the keys in the search field.
	focus = ui.state("fork.search.focus", 0),
}

-- The Explorer's Code view, opened on the chat's checkout (a folder: only
-- in it, its filters showing).
function M.show(folder: string?)
	M.folder:set(folder or "")
	if folder and folder ~= "" then
		M.filters_open:set(true)
	end
	M.mode:set("code")
	M.focus:set(M.focus:peek() + 1)
	require("@app/screens/panel").show_files()
end

-- (Once: see screens/sidebar's same-checkout command.)
pcall(ui.command, {id="fork.search.workspace",title="Find in Files",shortcut="cmd+shift+f",run=function() M.show() end})
return M
