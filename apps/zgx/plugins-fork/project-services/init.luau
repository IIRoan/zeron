--!strict
local zeron = require("./fork_sdk")(require("./zeron")("project-services"), "project-services")
local Model = require("./model")(zeron)
local View = require("./view")(zeron, Model)
local PanelHome = require("@ds/panel_home")
local Setup = require("./editor")(zeron,Model).Setup

-- Foreground chat selection owns the checkout handoff. Polling and opening
-- settings never activates an environment; shells can stay selected below.
local desired: any=nil;local last="";local handing_off=false
local function handoff()
    if handing_off or not desired or last==desired.key then return end
    local context=desired;last=context.key;handing_off=true
    local m=Model.get(context)
    local pending=table.clone(m.pending:peek());pending["*"]=true;m.pending:set(pending);m.epoch+=1
    local function settled(error: any?)
        handing_off=false;m.epoch+=1
        local next=table.clone(m.pending:peek());next["*"]=nil;m.pending:set(next)
        if error then m.error:set(Model.message(error)) end
        m.refresh();handoff()
    end
    zeron.workbench("ControlProjectTerminals",context,{action="activate"}):after(function() settled() end,settled)
end
ui.effect(function()
    local route=zeron.read("route");local window=zeron.window()
    local session=zeron.read("session")
    desired=if route and route.name=="chat" and window.shown and window.active and session then Model.context(session,nil) else nil
    if not desired then last="" end
    handoff()
end)

-- Its worktree settings, a section of the project's page in Settings →
-- Projects (Linux Workbench's).
zeron.place {at="slot:linux-workbench.project-settings",after=function(): Node
    local project=zeron.context("project")
    local context=Model.context(nil,if type(project)=="table" then project.id else project);if not context then return ui.div {} end
    context.key ..= ":settings-page"
    return Setup {model=Model.get(context)}
end}

zeron.tab("services", function(tab: any): Node
    return View { session = tab.data and tab.data.session, project = tab.data and tab.data.project }
end)

local function open(panel: any, session: string?, project: string?)
    panel.open_tab { kind = "services", title = "Services", icon = "terminal",
        data = {session = session, project = project} }
end

zeron.place { at = "hook:panel.empty", after = function(): Node
    local session = zeron.context("session")
    local panel = zeron.context("panel")
    if not panel or not session or not session.project then return ui.div {} end
    return PanelHome.Choice { id = "services-open", icon = "terminal", label = "Services", key = "S",
        on_click = function() open(panel, session.id, session.project) end }
end }

