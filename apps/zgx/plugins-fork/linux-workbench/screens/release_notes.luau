-- Fork-aware, offline notes, with the same acknowledgement rules as GPUI.
local ds=require("@ds")
local Markdown=require("@ds/markdown")
local Prefs=require("./prefs")
local catalog=require("./release_data")
local about=ui.shared("about",{} :: any)
local auto=Prefs.persist("release_notes.show_after_updates",true)
local seen=Prefs.persist("release_notes.seen",{} :: any)
local M={open=ui.state("fork.release_notes.open",false),selected=ui.state("fork.release_notes.selected",1)}
local function digest(r: any): string return r.digest .. ":workbench-1.1.0" end
local function current(): number
    local version=about:get().engine_version
    for i,r in catalog.releases do if r.version==version then return i end end
    return 1
end
function M.show() M.selected:set(current());M.open:set(true) end
ui.command {id="fork.release_notes",title="What's new · release notes and changelog",run=M.show}
local function newer(a: string,b: string): boolean
    local x={};local y={}
    for n in string.gmatch(a,"%d+") do table.insert(x,tonumber(n)) end
    for n in string.gmatch(b,"%d+") do table.insert(y,tonumber(n)) end
    for i=1,math.max(#x,#y) do if (x[i] or 0)~=(y[i] or 0) then return (x[i] or 0)>(y[i] or 0) end end
    return false
end
local offered=false;local was=false
    ui.effect(function()
        local version=about:get().engine_version
        if offered or not version then return end
        offered=true
        local r=catalog.releases[current()];local previous=seen:peek()
        if auto:peek() and previous.digest~=digest(r) and not newer(previous.version or "",r.version) then
            M.show()
        end
    end)
    ui.effect(function()
        local showing=M.open:get()
        if was and not showing then local r=catalog.releases[current()];seen:set({digest=digest(r),version=r.version}) end
        was=showing
    end)

M.View=ui.component(function(): Node
    return ds.Dialog {open=M.open,title="What's new",size="lg",cancel="Close",
        ui.column {w_full=true,gap="$space.3",
            ui.row {w_full=true,gap="$space.2",
                ds.Button {"This update",tone="secondary",size="sm",on_click=function() M.selected:set(current()) end},
                ds.Button {"Previous release",tone="ghost",size="sm",disabled=function() return catalog.releases[current()+1]==nil end,
                    on_click=function() M.selected:set(current()+1) end}},
            ui.presence(function() return M.selected:get() end,function(index: number): Node
                local r=catalog.releases[index]
                local changes={"### Linux fork"};for _,s in r.fork_changes do table.insert(changes,`- {s}`) end
                if #r.fork_changes==0 then table.insert(changes,"No additional fork changes in this release.") end
                return ui.column {w_full=true,gap="$space.2",
                    ui.text {`Zeron {r.version}`,weight="$font.weight.semibold"},
                    ui.scroll {id="fork-release-notes",w_full=true,h=400,selectable=true,
                        ui.column {w_full=true,gap="$space.4",
                            if index==current() then Markdown {text="### Workbench plugins\n- Original Linux layout, folder navigation and advanced workspace search.\n- Full Git menus, selection, syntax highlighting and commit drafts.\n- Services in the left tools panel, shared databases, worktree settings and an optional shell terminal dock.\n\nThese changes are provided by three independent fork plugins."} else nil,
                            Markdown {text=table.concat(changes,"\n")},
                            Markdown {text=`### Upstream\n{if r.notes_source=="git_history" then "Notes from the bundled Git history.\n\n" else ""}{r.upstream_notes}`}}}}
            end),
            ds.Checkbox {value=auto,label="Show after updates"},
        }}
end)
return M
