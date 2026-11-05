--!strict
local Button = require("@ds/button")
local Icon = require("@ds/icon")
local IconButton = require("@ds/icon_button").IconButton
local FadeText = require("@ds/fade_text")
local Status = require("@ds/status")
local Dropdown = require("@ds/dropdown")
local ContextMenu = require("./ui/context_menu")
local Dialog = require("@ds/dialog").Dialog
local TextField = require("@ds/text_field")

return function(zeron: any, Model: any)
    local Editor = require("./editor")(zeron,Model)
    local Pane = ui.component(function(p: any): Node
        local m = Model.get(p.context)
        local box = ui.ref()
        local editor_kind = ui.use_state("services")
        local activate = ui.use_state(false)
        local terminal = ui.use_state(nil :: any)
        local output_error = ui.use_state("")
        local output_key = ""
        local filter=ui.use_state("")
        local open_actions: any=nil
        local action_button: any=nil
        ui.after(0,function() m.refresh() end)
        ui.every(2000,function() if (box.width or 0)>0 and (box.height or 0)>0 then m.refresh() end end)
        local function selected_service(): any?
            local snapshot=m.snapshot:get()
            for _,service in (snapshot and snapshot.config.services) or {} do
                if service.id==m.selected:get() then return service end
            end
            return nil
        end
        local function selected_run(): any?
            return Model.run(m.snapshot:get(),m.selected:get())
        end
        local function edit(kind: string)
            editor_kind:set(kind);m.error:set("");m.editor:set(true)
        end
        local function service_menu(service: any): {any}
            local id=service.id
            local running=Model.active(Model.run(m.snapshot:peek(),id))
            local busy=m.busy(id)
            return {
                {label="Start",icon="action-play",disabled=busy or running,on_select=function() m.control("start",id) end},
                {label="Stop",icon="stop",disabled=busy or not running,on_select=function() m.control("stop",id) end},
                {label="Restart",icon="refresh",disabled=busy,on_select=function() m.control("restart",id) end},
                {separator=true},
                {label="Show output",icon="terminal",on_select=function() m.selected:set(id) end},
                {label=if Model.database(service) then "Database settings" else "Service settings",icon="settings",on_select=function()
                    m.selected:set(id);edit(if Model.database(service) then "databases" else "services")
                end},
            }
        end
        local function ink(id: string): string
            local run=Model.run(m.snapshot:get(),id)
            if run and (run.status=="failed" or run.readiness=="notReady") then return "$status.danger" end
            if m.pending:get()[id] or m.pending:get()["*"] or (run and (run.status=="restarting" or run.readiness=="checking")) then return "$status.warning" end
            return if Model.active(run) then "$status.success" else "$text.tertiary"
        end
        local function state_label(id: string): string
            return if m.pending:get()[id] or m.pending:get()["*"] then "Working…" else Model.label(Model.run(m.snapshot:get(),id))
        end
        local function dot(id: string): Node
            return ui.div {w=10,h=10,shrink=0,items="center",justify="center",aria_hidden=true,
                ui.presence(function()
                    local run=Model.run(m.snapshot:get(),id)
                    return if m.pending:get()[id] or m.pending:get()["*"] or (run and (run.status=="restarting" or run.readiness=="checking")) then "busy" else "idle"
                end,function(kind: string): Node
                    if kind=="busy" then return Status.Spinner {size=10,ink="$status.warning"} end
                    return ui.div {w=7,h=7,rounded="$radius.full",bg=function() return ink(id) end}
                end),
            }
        end
        ui.effect(function()
            local snapshot=m.snapshot:get()
            local id=m.selected:get()
            local run=Model.run(snapshot,id)
            local key=if run and run.terminal then `{id}:{run.terminal.id}` else ""
            if key==output_key then return end
            output_key=key;terminal:set(nil);output_error:set("")
            if key~="" then
                m.call("ProjectServiceTerminal",{serviceId=id}):after(function(value: any)
                    if output_key==key then terminal:set(value) end
                end,function(e: any) if output_key==key then output_error:set(Model.message(e)) end end)
            end
        end)
        local visible=ui.computed(function()
            local snapshot=m.snapshot:get();local list={};local q=string.lower(filter:get())
            for _,service in (snapshot and snapshot.config.services) or {} do
                if q=="" or string.find(string.lower(service.name .. " " .. service.command),q,1,true) then table.insert(list,service) end
            end
            return list
        end)
        local rows: {[string]: any}={}
        local function step(e: any): boolean
            if e.cmd or e.ctrl or e.alt then return false end
            local key=string.lower(e.key);local list=visible:peek();if #list==0 then return false end
            local n=table.find({"arrowdown","arrowup","home","end"},key);if not n then return false end
            local at=1;for i,service in list do if service.id==m.selected:peek() then at=i end end
            at=if key=="home" then 1 elseif key=="end" then #list else math.clamp(at+(if key=="arrowdown" then 1 else -1),1,#list)
            local id=list[at].id;m.selected:set(id)
            if rows[id] then rows[id]:focus() end
            return true
        end
        local function service_list(wide: boolean): Node
            return ui.column {id="services-sidebar",w=if wide then 220 else nil,w_full=not wide,h_full=wide,
                h=if wide then nil else function() return math.min(250,math.max(96,#visible:get()*46+48)) end,
                min_h=0,shrink=0,border_sides=if wide then {0,1,0,0} else {0,0,1,0},border_color="$border.subtle",
                ui.div {w_full=true,p="$space.2",TextField {value=filter,label="Filter services",placeholder="Filter services",size="sm",w_full=true}},
                ui.scroll {id="services-list",w_full=true,grow=1,min_h=0,on_key=step,
                    ui.column {w_full=true,p="$space.1",gap="$space.1",
                        ui.each(visible,function(service: any,index: number): Node
                            return ContextMenu {w_full=true,items=function() return service_menu(service) end,
                                ui.button {role="tab",label=service.name,w_full=true,min_w=0,h=42,selected=function() return m.selected:get()==service.id end,
                                    px="$space.3",py="$space.2",rounded="$radius.sm",justify="start",gap="$space.2",
                                    on_mount=function(el: any) rows[service.id]=el end,
                                    bg=function() return if m.selected:get()==service.id then "$zeron.secondary.bg" else "#00000000" end,
                                    hover={bg="$hover"},on_click=function() m.selected:set(service.id) end,
                                    ui.column {grow=1,min_w=0,gap="$space.1",
                                        ui.row {w_full=true,items="center",gap="$space.2",
                                            dot(service.id),FadeText {service.name,grow=1,weight="$font.weight.medium",text_size="$font.size.sm"},
                                            if Model.database(service) then ui.text {"Shared",text_size="$font.size.xs",color="$text.tertiary"} else nil},
                                        ui.row {w_full=true,items="center",gap="$space.2",pl=ui.raw(15,"align status with the service name"),
                                            ui.text {function() return state_label(service.id) end,color=function() return ink(service.id) end,text_size="$font.size.xs"},
                                            if wide then FadeText {service.command,grow=1,font="$font.mono",text_size="$font.size.xs",color="$text.tertiary"} else nil}}}}
                        end,{key=function(service: any) return service.id end}),
                        ui.show(function() return #visible:get()==0 end,ui.text {"No matching services",p="$space.3",wrap=true,color="$text.secondary",text_size="$font.size.sm"})}}}
        end
        local function output(): Node
            return ui.column {w_full=true,h_full=true,grow=1,min_w=0,min_h=0,
                ui.show(function() local s=m.snapshot:get();return s~=nil and #s.config.services>0 end,
                ui.row {id="service-output-toolbar",w_full=true,h=42,shrink=0,px="$space.3",gap="$space.2",items="center",border_sides={0,0,1,0},border_color="$border.subtle",
                    Icon {name="terminal",size=14,color="$text.tertiary"},
                    FadeText {function() local s=selected_service();return if s then s.name else "Output" end,grow=1,weight="$font.weight.medium",text_size="$font.size.sm"},
                    ui.text {function() return state_label(m.selected:get()) end,text_size="$font.size.xs",color=function() return ink(m.selected:get()) end},
                    Button {function() return if Model.active(selected_run()) then "Stop" else "Start" end,size="xs",tone="secondary",
                        busy=function() return m.pending:get()[m.selected:get()]==true or m.pending:get()["*"]==true end,
                        disabled=function() return m.selected:get()=="" or m.busy(m.selected:get()) end,
                        on_click=function() local id=m.selected:peek();m.control(if Model.active(Model.run(m.snapshot:peek(),id)) then "stop" else "start",id) end},
                    IconButton {icon="refresh",label="Restart service",size="sm",disabled=function() return m.selected:get()=="" or m.busy(m.selected:get()) end,
                        on_click=function() m.control("restart",m.selected:peek()) end},
                    IconButton {icon="settings",label="Selected service settings",size="sm",disabled=function() return m.selected:get()=="" end,
                        on_click=function() local s=selected_service();if s then edit(if Model.database(s) then "databases" else "services") end end},
                }),
                ui.presence(function()
                    local snapshot=m.snapshot:get()
                    if not snapshot then return "loading" end
                    if #snapshot.config.services==0 then return "empty" end
                    local t=terminal:get()
                    return if t then `terminal:{t.id}` else "stopped"
                end,function(kind: string): Node
                    if kind=="loading" then return ui.row {grow=1,w_full=true,justify="center",items="center",gap="$space.2",
                        Status.Spinner {size=14},ui.text {"Loading services…",color="$text.secondary"}} end
                    if kind=="empty" then return ui.column {grow=1,w_full=true,items="center",justify="center",gap="$space.3",p="$space.4",
                        Icon {name="terminal",size=24,color="$text.tertiary"},
                        ui.text {"Run your project here",weight="$font.weight.medium"},
                        ui.text {"Add the commands you use for development, then start them together.",color="$text.secondary",wrap=true,max_w=340,align="center",text_size="$font.size.sm"},
                        Button {"Add a service",size="sm",on_click=function() edit("services") end},
                    } end
                    local t=terminal:peek()
                    if t then
                        local term=(ui :: any).term(`services:{p.context.key}:{t.id}`,{command=t.command,cwd="~"})
                        return (ui :: any).terminal {id="service-output",term=term,label="Service output",relative=true,w_full=true,grow=1,min_h=0}
                    end
                    return ui.column {id="service-stopped",w_full=true,grow=1,items="center",justify="center",gap="$space.2",p="$space.4",
                        Icon {name="terminal",size=24,color="$text.tertiary"},
                        ui.text {function() local s=selected_service();return if s then `{s.name} is stopped` else "Select a service" end,weight="$font.weight.medium"},
                        ui.text {function() local s=selected_service();return if s then s.command else "" end,font="$font.mono",text_size="$font.size.sm",color="$text.secondary",wrap=true,max_w=480,align="center"},
                        ui.text {function() local run=selected_run();return if output_error:get()~="" then output_error:get() elseif run then run.message or (if run.exitCode~=nil then `Exit code {run.exitCode}` else "") else "" end,color="$text.secondary",wrap=true,text_size="$font.size.xs"},
                    }
                end,{w_full=true,grow=1,min_h=0}),
            }
        end
        return ui.column {ref=box,id="project-services",w_full=true,h_full=true,min_h=0,
            ui.row {id="services-toolbar",w_full=true,h=42,shrink=0,px="$space.3",gap="$space.2",items="center",border_sides={0,0,1,0},border_color="$border.subtle",
                ui.text {"Services",weight="$font.weight.semibold",text_size="$font.size.sm"},
                ui.div {grow=1,min_w=0,ui.show(function() return (box.width or 0)>=520 end,FadeText {p.context.name,w_full=true,text_size="$font.size.xs",color="$text.secondary"})},
                ui.show(function() return (box.width or 0)>=650 end,ui.text {function()
                    local snapshot=m.snapshot:get();if not snapshot or #snapshot.config.services==0 then return "" end
                    local active=0;for _,run in snapshot.runs do if Model.active(run) then active+=1 end end
                    return `{active}/{#snapshot.config.services} running`
                end,text_size="$font.size.xs",color="$text.secondary"}),
                Button {"Edit services",tone="ghost",size="xs",on_click=function() edit("services") end},
                ContextMenu {hook=function(show: any) open_actions=show end,items=function() return {
                    {label="Start all",icon="action-play",disabled=m.busy(nil),on_select=function() m.control("start",nil) end},
                    {label="Stop app services",icon="stop",disabled=m.busy(nil),on_select=function() m.control("stop-worktree",nil) end},
                    {label="Stop all, including databases",icon="stop",disabled=m.busy(nil),on_select=function() m.control("stop",nil) end},
                    {label="Restart all",icon="refresh",disabled=m.busy(nil),on_select=function() m.control("restart",nil) end},
                    {separator=true},
                    {label="Edit services…",icon="settings",on_select=function() edit("services") end},
                    {label="Shared databases",icon="remote-server",on_select=function() edit("databases") end},
                    {label="Project settings…",icon="worktree",on_select=function() edit("setup") end},
                    {label="Refresh",icon="refresh",on_select=function() m.refresh() end},
                } end,
                    IconButton {icon="more-horizontal",label="Service actions",size="sm",on_mount=function(el: any) action_button=el end,on_click=function()
                        if open_actions and action_button then open_actions(action_button.x,action_button.y+action_button.height) end
                    end},
                },
            },
            ui.show(function() local s=m.snapshot:get();return s~=nil and s.checkout~=nil and s.checkout~=p.context.scope.cwd end,
                ui.row {w_full=true,px="$space.3",py="$space.1",gap="$space.2",items="center",
                    ui.text {"Services are running in another checkout.",grow=1,text_size="$font.size.xs",color="$status.warning",wrap=true},
                    Button {"Use this checkout",size="xs",disabled=function() return m.busy(nil) end,on_click=function() activate:set(true) end}}),
            ui.show(function() return m.error:get()~="" end,ui.text {function() return m.error:get() end,px="$space.3",py="$space.2",wrap=true,color="$status.danger",text_size="$font.size.sm"}),
            ui.presence(function() return if (box.width or 0)>=580 then "wide" else "narrow" end,function(kind: string): Node
                return (if kind=="wide" then ui.row else ui.column) {w_full=true,grow=1,min_h=0,
                    ui.show(function() local s=m.snapshot:get();return s~=nil and #s.config.services>0 end,service_list(kind=="wide")),output()}
            end,{w_full=true,grow=1,min_h=0}),
            Editor {model=m,kind=function() return editor_kind:get() end},
            Dialog {open=activate,title="Move services to this checkout?",body="App services in the previous checkout will stop. Shared databases stay in the main project folder.",
                confirm={label="Use this checkout",on_click=function() m.control("activate",nil) end}},
        }
    end)
    return ui.component(function(p: any): Node
        local project=ui.persist("services.selected_project","")
        local function chosen(): string?
            if p.project then return p.project end
            local list=zeron.projects()
            if list[project:get()] then return project:get() end
            local ids={};for id in list do table.insert(ids,id) end;table.sort(ids)
            return ids[1]
        end
        ui.effect(function()
            if p.choose_project then
                local id=chosen() or ""
                if project:peek()~=id then project:set(id) end
            end
        end)
        return ui.column {w_full=true,h_full=true,min_h=0,
            -- Services remains independent of the layout's popup styling.
            theme={ ["zeron.menu.tint"]="alpha({zeron.menu.solid}, 1)", ["zeron.menu.blur"]=0,
                ["menu.bg"]="{zeron.menu.tint}" },
            if p.choose_project then ui.row {w_full=true,p="$space.2",gap="$space.2",items="center",
                ui.text {"Project",color="$text.secondary",text_size="$font.size.sm"},
                Dropdown {value=project,placeholder="Choose a project",items=function()
                    local items={};for id,record in zeron.projects() do table.insert(items,{value=id,label=record.name or id}) end
                    table.sort(items,function(a: any,b: any) return a.label<b.label end);return items
                end},
            } else nil,
            ui.presence(function() local c=Model.context(p.session,chosen());return if c then c.key else "" end,function(key: string): Node
                if key=="" then return ui.text {"Choose a project to manage its services.",p="$space.3",wrap=true,color="$text.secondary"} end
                return Pane {context=Model.context(p.session,chosen())}
            end,{w_full=true,grow=1,min_h=0}),
        }
    end)
end
