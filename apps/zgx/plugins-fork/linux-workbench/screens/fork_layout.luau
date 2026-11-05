local Prefs=require("./prefs")
-- The Linux fork's workbench: tools at the left, project chats at the
-- right. Services is a left tool; ordinary shells have an optional dock.
local ds=require("@ds")
local route=require("@app/screens/shell/route")
local M={terminal=Prefs.persist("fork.terminal_dock",false),height=Prefs.persist("fork.terminal_height",280)}
local plugins=ui.shared("plugins",{list={}} :: any)
function M.enabled(id: string): boolean
    for _,plugin in plugins:get().list or {} do if plugin.id==id then return plugin.status=="on" end end
    return false
end
function M.Activity(): Node
    local function panel(): any return require("@app/screens/panel") end
    local function git(which: string)
        local session=route.session:peek();if not session then return end
        panel().open_plugin_tab(session,{plugin="zeron-git",kind=`git-{which}`,title=if which=="history" then "History" else "Source control",data={session=session.id}})
    end
    return ui.column {id="fork-activity",w=44,h_full=true,shrink=0,pt=ui.raw(42,"align tools below the title controls"),items="center",gap="$space.3",border_sides={0,1,0,0},border_color="$border.subtle",
        ds.IconButton {icon="folder",label="Explorer",disabled=function() return route.session:get()==nil end,tooltip="Explorer · open a chat to use its checkout",on_click=function() panel().show_files() end},
        ds.IconButton {icon="magnifer",label="Find in Files",disabled=function() return route.session:get()==nil end,tooltip="Find in Files · open a chat to search its checkout",on_click=function() require("./files_search").show() end},
        ui.show(function() return M.enabled("zeron-git") end,ds.IconButton {icon="git-branch",label="Source control",disabled=function() return route.session:get()==nil end,tooltip="Source control · open a chat to use its checkout",on_click=function() git("changes") end}),
        ui.show(function() return M.enabled("zeron-git") end,ds.IconButton {icon="git-commit",label="Git history",disabled=function() return route.session:get()==nil end,tooltip="Git history · open a chat to use its checkout",on_click=function() git("history") end}),
        ui.show(function() return M.enabled("project-services") end,ds.IconButton {icon="remote-server",label="Project services",on_click=function()
            local session=route.session:peek()
            if not session then route.plugin_page("project-services","services");return end
            panel().open_plugin_tab(session,{plugin="project-services",kind="services",title="Services",data={session=session.id,project=session.project}})
        end}),
        ds.IconButton {icon="terminal",label="Shell terminal",disabled=function() return route.session:get()==nil end,tooltip="Shell terminal · open a chat to use its checkout",on_click=function() M.terminal:set(not M.terminal:peek()) end},
    }
end
function M.Dock(props: any): Node
    local from_y,from_h=0,0;local dragging=false
    local function limit(): number return math.max(160,(props.room.height or 800)*0.55) end
    local function height(): number return math.clamp(M.height:get(),160,limit()) end
    return ui.show(function() return M.terminal:get() and route.session:get()~=nil end,ui.column {
        id="fork-terminal-container",w_full=true,h=height,shrink=0,relative=true,overflow="hidden",
        ui.div {h=5,w_full=true,cursor="row_resize",bg="$border.subtle",label="Resize terminal",
            on_press=function(e: any) dragging=true;from_y=e.y;from_h=height() end,
            on_pointer_move=function(e: any) if dragging then M.height:set(math.clamp(from_h+from_y-e.y,160,limit())) end end,
            on_release=function() dragging=false end,
            on_double_click=function() M.height:set(280) end},
        require("./terminal_dock") {expanded=M.terminal},
        ui.div {absolute=true,top=12,right=8,
            ds.IconButton {icon="alt-arrow-down",label="Hide terminals",size="sm",on_click=function() M.terminal:set(false) end}},
    })
end
return M
