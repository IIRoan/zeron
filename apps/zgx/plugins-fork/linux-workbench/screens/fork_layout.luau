local Prefs=require("./prefs")
-- The Linux fork's workbench: tools at the left, project chats at the
-- right. Services is a left tool; ordinary shells have an optional dock.
local ds=require("@ds")
local route=require("@app/screens/shell/route")
local M={terminal=Prefs.persist("fork.terminal_dock",false),height=Prefs.persist("fork.terminal_height",280),open=ui.state("fork.terminal_open",0)}
local plugins=ui.shared("plugins",{list={}} :: any)
function M.enabled(id: string): boolean
    for _,plugin in plugins:get().list or {} do if plugin.id==id then return plugin.status=="on" end end
    return false
end
-- The chat list's width as it shows (none while folded, hidden for room,
-- or under settings).
function M.list_w(): number
    local fit=require("@app/screens/shell/fit");local view=require("@app/screens/shell/view")
    if view.collapsed:get() or not fit.current:get().sidebar or route.current:get().name=="settings" then return 0 end
    -- (Its width as the list keeps it (screens/sidebar), not its module:
    -- loaded from here, it would declare its commands twice.)
    return math.min(Prefs.persist("shell_sidebar_width",256):get(),fit.sidebar_room:get())
end
-- The title bar's box at the window's right (screens/title_bar): over the
-- chat list, or, folded, what its controls, the page's and the window's
-- buttons take (the panel toggle's 24 and its gap the page's).
function M.bar_w(): number
    return math.max(M.list_w(),ds.TitleBar.controls_end()+ds.TitleBar.actions_right()+32)
end
-- The tools rail: the theme's background, a hairline at its right, its
-- tools from under the title bar's line down (the desktop app's 44 wide,
-- a tool 44 high).
function M.Activity(): Node
    local function panel(): any return require("@app/screens/panel") end
    local function git(which: string)
        local session=route.session:peek();if not session then return end
        panel().open_plugin_tab(session,{plugin="zeron-git",kind=`git-{which}`,title=if which=="history" then "History" else "Source control",data={session=session.id}})
    end
    return ui.column {id="fork-activity",w=44,h_full=true,shrink=0,items="center",gap=0,bg="$zeron.window",border_sides={0,1,0,0},border_color="$border",
        theme={["zeron.icon_button.md"]=44},
        -- The mark at its top, as tall as the title bar, a hairline under
        -- it (the desktop app's workbench brand).
        ui.div {id="fork-brand",w=44,h=ui.raw(38,"the title bar's line"),shrink=0,items="center",justify="center",
            border_sides={0,0,1,0},border_color="$border",role="img",label="Solace",tooltip="Solace",tooltip_placement={side="right",align="center",offset=6},
            ui.svg {src=`code:{require("../ui/solace_logo")}`,w=26,h=26,tint="$text.primary",aria_hidden=true}},
        ds.IconButton {icon="folder",label="Explorer",disabled=function() return route.session:get()==nil end,tooltip="Explorer · open a chat to use its checkout",on_click=function() panel().show_files() end},
        ds.IconButton {icon="magnifer",label="Find in Files",disabled=function() return route.session:get()==nil end,tooltip="Find in Files · open a chat to search its checkout",on_click=function() require("./files_search").show() end},
        ui.show(function() return M.enabled("zeron-git") end,ds.IconButton {icon="git-branch",label="Source control",disabled=function() return route.session:get()==nil end,tooltip="Source control · open a chat to use its checkout",on_click=function() git("changes") end}),
        ui.show(function() return M.enabled("project-services") end,ds.IconButton {icon="remote-server",label="Project services",on_click=function()
            local session=route.session:peek()
            if not session then route.plugin_page("project-services","services");return end
            panel().open_plugin_tab(session,{plugin="project-services",kind="services",title="Services",data={session=session.id,project=session.project}})
        end}),
        -- The project's environment (the Linux fork's Sandbox and Windows VM
        -- plugins): their tabs beside the chat.
        ui.show(function() return M.enabled("project-sandbox") end,ds.IconButton {icon="shield-check",label="Sandbox",disabled=function() return route.session:get()==nil end,tooltip="Sandbox · open a chat to see its project's sandbox",on_click=function()
            local session=route.session:peek();if not session then return end
            panel().open_plugin_tab(session,{plugin="project-sandbox",kind="sandbox",title="Sandbox",data={session=session.id,project=session.project}})
        end}),
        ui.show(function() return M.enabled("windows-vm") end,ds.IconButton {icon="monitor",label="Windows VM",disabled=function() return route.session:get()==nil end,tooltip="Windows VM · open a chat to see its project's Windows machine",on_click=function()
            local session=route.session:peek();if not session then return end
            panel().open_plugin_tab(session,{plugin="windows-vm",kind="windows-vm",title="Windows VM",data={session=session.id,project=session.project}})
        end}),
        ds.IconButton {icon="terminal",label="Shell terminal",disabled=function() return route.session:get()==nil end,tooltip="Shell terminal · open a chat to use its checkout",on_click=function() M.terminal:set(not M.terminal:peek()) end},
    }
end
function M.Dock(props: any): Node
    local from_y,from_h=0,0;local dragging=false
    local function limit(): number return math.max(160,(props.room.height or 800)*0.55) end
    local function height(): number return math.clamp(M.height:get(),160,limit()) end
    -- How far open it is, 0 to 1: eased as it opens and closes, as the
    -- shell's own columns are (@ds/motion, at once with motion reduced); a
    -- drag sizes it at once.
    local motion=require("@ds/motion")
    local easing: any=nil;local first=true
    ui.effect(function()
        local to=if M.terminal:get() and route.session:get()~=nil then 1 else 0
        if easing then easing:cancel();easing=nil end
        -- (As it was left, at once, when the window opens.)
        if first or motion.reduced_now() then M.open:set(to) else easing=anime.animate(M.open,{value=to,duration=motion.BASE,ease=motion.EASE}) end
        first=false
    end)
    return ui.show(function() return M.open:get()>0 end,ui.column {
        id="fork-terminal-container",w_full=true,h=function() return height()*M.open:get() end,shrink=0,relative=true,overflow="hidden",
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
