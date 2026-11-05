--!strict
local Button = require("@ds/button")
local TextField = require("@ds/text_field")
local TextArea = require("@ds/text_area")
local Checkbox = require("@ds/checkbox")
local Dialog = require("./ui/dialog")
local Dropdown = require("@ds/dropdown")
local Icon = require("@ds/icon")
local FadeText = require("@ds/fade_text")

return function(zeron: any, Model: any)
    local Field = ui.component(function(p: any): Node
        local value = ui.use_state(tostring(p.value or ""))
        ui.effect(function() p.changed(value:get()) end)
        return ui.column {gap="$space.1", w_full=true,
            if p.multiline then TextArea {value=value,label=p.label,rows=3,max_rows=8}
            else TextField {value=value,label=p.label,placeholder=p.placeholder or "",w_full=true,font=if p.code then "$font.mono" else "$font.sans"},
            if p.description then ui.text {p.description,wrap=true,color="$text.tertiary",text_size="$font.size.xs"} else nil,
        }
    end)
    local Toggle = ui.component(function(p: any): Node
        local value = ui.use_state(p.value == true)
        ui.effect(function() p.changed(value:get()) end)
        return Checkbox {value=value,label=p.label}
    end)
    local Editor: any=nil
    local Worktrees=require("./worktrees")(zeron,Model,Field,Toggle,function(model: any,done: any): Node return Editor {model=model,kind="services",on_saved=done} end)
    Editor=ui.component(function(p: any): Node
        local m = p.model
        local worktrees=Worktrees(m)
        local open = m.editor
        local mode = ui.use_state("services")
        local draft = ui.use_state({} :: {any})
        local setup = ui.use_state(nil :: any)
        local settings = ui.use_state(nil :: any)
        local original = ui.use_state("")
        local error_text = ui.use_state("")
        local discard = ui.use_state(false)
        local epoch = ui.use_state(0)
        local serial = 0
        local loading = 0
        local selected = ui.use_state(m.selected:peek())
        local function select_current(items: {any})
            local wanted=m.selected:peek()
            for _,item in items do if item.id==wanted then selected:set(wanted);return end end
            selected:set(if items[1] then items[1].id else "")
        end
        local function value(): any
            return if mode:peek() == "setup" then settings:peek() else draft:peek()
        end
        local function dirty(): boolean
            if mode:peek()=="setup" then return worktrees.dirty() end
            return original:peek() ~= "" and ui.json.encode(value()) ~= original:peek()
        end
        local function close()
            if m.saving:peek() then return end
            if dirty() then discard:set(true) else open:set(false) end
        end
        local function load(kind: string)
            loading+=1
            local request=loading
            mode:set(kind); error_text:set("")
            original:set("");draft:set({});settings:set(nil);setup:set(nil)
            if kind=="setup" then original:set("worktrees");worktrees.load();return end
            local snapshot = m.snapshot:peek()
            if kind == "services" then
                local function services_loaded(result: any)
                    if request~=loading or not open:peek() then return end
                    local items = {}
                    for _,s in result.config.services do if not Model.database(s) then table.insert(items,Model.clone(s)) end end
                    m.snapshot:set(result)
                    draft:set(items);select_current(items);original:set(ui.json.encode(items));epoch:add(1)
                end
                if snapshot then services_loaded(snapshot)
                else m.call("GetProjectTerminals"):after(services_loaded,function(e: any)
                    if request==loading and open:peek() then error_text:set(Model.message(e)) end
                end) end
            else
                local context = if kind == "databases" then Model.context(nil,m.context.project) else m.context
                zeron.workbench("GetWorktreeSettings",context,{}):after(function(result: any)
                    if request~=loading or not open:peek() then return end
                    setup:set(result)
                    if kind == "databases" then draft:set(Model.clone(result.databases or {}));select_current(draft:peek());original:set(ui.json.encode(draft:peek()))
                    else settings:set(Model.clone(result.settings));original:set(ui.json.encode(settings:peek())) end
                    epoch:add(1)
                end,function(e: any) error_text:set(Model.message(e)) end)
            end
        end
        local was = false
        ui.effect(function()
            local showing = open:get()
            if showing and not was then load(if type(p.kind)=="function" then p.kind() else p.kind or "services") end
            was = showing
        end)
        local function change(id: string, key: string, value: any)
            if key=="timeout" then value=tonumber(value) or 0 end
            local current = draft:peek()
            local index=nil;for i,item in current do if item.id==id then index=i;break end end
            if not index then return end
            if not current[index] then return end
            local old = if key == "readiness" then current[index].setupPrerequisite and current[index].setupPrerequisite.command
                elseif key == "timeout" then current[index].setupPrerequisite and current[index].setupPrerequisite.timeoutSeconds
                else current[index][key]
            if old == value then return end
            local next = Model.clone(current)
            if key == "readiness" then next[index].setupPrerequisite.command = value
            elseif key == "timeout" then next[index].setupPrerequisite.timeoutSeconds = tonumber(value) or 0
            else next[index][key] = value end
            draft:set(next)
        end
        local function change_setting(key: string, value: any)
            local now = settings:peek()
            if not now or (now[key]==nil and value=="") or ui.json.encode(now[key]) == ui.json.encode(value) then return end
            local next = Model.clone(now);next[key] = value;settings:set(next)
        end
        local function add()
            serial += 1
            local list = Model.clone(draft:peek())
            local s: any = {id=`service-{os.time()}-{serial}`,name=if mode:peek()=="databases" then "Database" else "New service",command="",directory="",restartOnFailure=false}
            if mode:peek()=="databases" then s.setupPrerequisite={command="",timeoutSeconds=120} end
            table.insert(list,s);draft:set(list)
            selected:set(s.id)
        end
        local function save()
            if mode:peek()=="setup" then worktrees.save();return end
            if original:peek()=="" then return end
            error_text:set("")
            if mode:peek() ~= "setup" then
                for _, s in draft:peek() do
                    if not string.find(s.name or "","%S") or not string.find(s.command or "","%S") then error_text:set("Give every service a name and command.");return end
                    if s.setupPrerequisite and (not string.find(s.setupPrerequisite.command or "","%S") or (s.setupPrerequisite.timeoutSeconds or 0)<=0) then
                        error_text:set("Shared databases need a readiness command and a positive timeout.");return
                    end
                end
            end
            local function done() original:set(ui.json.encode(value()));open:set(false);m.refresh();if p.on_saved then p.on_saved() end end
            if mode:peek()=="services" then
                local services = {}
                for _, s in (m.snapshot:peek().config.services) do if Model.database(s) then table.insert(services,Model.clone(s)) end end
                for _, s in draft:peek() do table.insert(services,s) end
                m.save(services,done)
            else
                if m.busy(nil) then return end
                local context = if mode:peek()=="databases" then Model.context(nil,m.context.project) else m.context
                local params = {settings = if mode:peek()=="databases" then setup:peek().defaults else settings:peek(),
                    databases = if mode:peek()=="databases" then draft:peek() else ui.null}
                m.saving:set(true)
                zeron.workbench("SaveWorktreeSettings",context,params):after(function(_result: any)
                    m.saving:set(false);done()
                end,function(e: any) m.saving:set(false);error_text:set(Model.message(e)) end)
            end
        end
        local function cards(): Node
            local function current(): any?
                local first=nil
                for _,service in draft:get() do
                    first=first or service
                    if service.id==selected:get() then return service end
                end
                return first
            end
            return ui.column {w_full=true,gap="$space.4",
                ui.text {if mode:get()=="databases" then "Shared by every checkout. These commands run in the main project folder."
                    else "Development commands for this checkout. Configure shared databases from the Services menu.",wrap=true,color="$text.secondary",text_size="$font.size.sm"},
                ui.row {w_full=true,items="center",gap="$space.2",
                    ui.text {if mode:get()=="databases" then "Databases" else "App services",grow=1,weight="$font.weight.semibold",text_size="$font.size.sm"},
                    Button {if mode:get()=="databases" then "Add database" else "Add service",tone="secondary",size="sm",disabled=function() return original:get()=="" end,on_click=add},
                },
                ui.column {w_full=true,gap="$space.1",
                    ui.each(draft,function(s: any): Node
                        return ui.button {role="tab",label=function()
                            for _,service in draft:get() do if service.id==s.id then return `Edit {service.name}` end end;return "Edit service"
                        end,w_full=true,h=42,px="$space.3",gap="$space.2",items="center",justify="start",rounded="$radius.sm",
                            selected=function() local c=current();return c~=nil and c.id==s.id end,
                            bg=function() local c=current();return if c and c.id==s.id then "$zeron.secondary.bg" else "#00000000" end,
                            hover={bg="$hover"},on_click=function() selected:set(s.id) end,
                            Icon {name=if s.setupPrerequisite then "remote-server" else "terminal",size=16,color="$text.tertiary"},
                            FadeText {function() for _,service in draft:get() do if service.id==s.id then return service.name end end;return "" end,grow=1,text_size="$font.size.sm",weight="$font.weight.medium"},
                            Icon {name="alt-arrow-right",size=12,color="$text.tertiary"},
                        }
                    end,{key=function(s: any) return s.id end}),
                    ui.show(function() return #draft:get()==0 end,ui.column {w_full=true,py="$space.5",gap="$space.2",items="center",
                        Icon {name="terminal",size=24,color="$text.tertiary"},
                        ui.text {if mode:get()=="databases" then "No shared databases yet" else "No app services yet",color="$text.secondary",text_size="$font.size.sm"},
                    }),
                },
                ui.presence(function() local c=current();return if c then `{epoch:get()}:{c.id}` else "" end,function(key: string): Node
                    local s=current();if key=="" or not s then return ui.div {} end
                    return ui.column {id="service-settings-detail",w_full=true,gap="$space.4",pt="$space.4",border_sides={1,0,0,0},border_color="$border.subtle",
                        Field {label="Service name",placeholder="e.g. Web app",value=s.name,changed=function(v: string) change(s.id,"name",v) end},
                        Field {label="Command",placeholder="e.g. pnpm dev",code=true,value=s.command,description="Runs in a persistent terminal. Output stays available in Services.",changed=function(v: string) change(s.id,"command",v) end},
                        Field {label="Directory",placeholder="Project root",code=true,value=s.directory,description="A path relative to the project folder. Leave empty to use the root.",changed=function(v: string) change(s.id,"directory",v) end},
                        Toggle {label="Restart on failure",value=s.restartOnFailure,changed=function(v: boolean) change(s.id,"restartOnFailure",v) end},
                        if s.setupPrerequisite then ui.column {w_full=true,gap="$space.3",pt="$space.3",border_sides={1,0,0,0},border_color="$border.subtle",
                            ui.text {"Readiness",weight="$font.weight.semibold",text_size="$font.size.sm"},
                            Field {label="Readiness command",code=true,value=s.setupPrerequisite.command,description="App services wait until this command succeeds.",changed=function(v: string) change(s.id,"readiness",v) end},
                            Field {label="Readiness timeout (seconds)",value=s.setupPrerequisite.timeoutSeconds,changed=function(v: string) change(s.id,"timeout",v) end},
                        } else nil,
                        ui.row {w_full=true,justify="end",pt="$space.2",
                            Button {"Remove service",tone="ghost",size="sm",color="$status.danger",on_click=function()
                                local list=Model.clone(draft:peek());for i,item in list do if item.id==s.id then table.remove(list,i);break end end;draft:set(list)
                            end},
                        },
                    }
                end),
            }
        end
        local function setup_fields(): Node return worktrees.View() end
        return ui.div {
            Dialog.Sheet {open=open,size=560,title=function() return if mode:get()=="databases" then "Shared databases" elseif mode:get()=="setup" then "Project settings" else "Project services" end,
                dismissable=false,on_close=close,
                foot=ui.row {w_full=true,justify="end",gap="$space.2",
                    Button {"Cancel",tone="secondary",disabled=function() return m.saving:get() end,on_click=close},
                    if m.context.checkoutPath or m.context.scope.cwd~=m.context.root then ui.show(function() return mode:get()~="setup" end,Button {"Use project services",tone="secondary",disabled=function() return m.busy(nil) end,on_click=function() m.inherit(function() original:set("");open:set(false) end) end}) else nil,
                    Button {"Save",busy=m.saving,disabled=function() return m.saving:get() or original:get()=="" or (mode:get()=="setup" and worktrees.busy:get()) end,on_click=save},
                },
                ui.column {w_full=true,gap="$space.3",pointer_events=function() return if m.saving:get() then "none" else "auto" end,on_key=function(e: any) if e.key=="Escape" then close();return true end;return false end,
                    ui.presence(function() return `{mode:get()}:{epoch:get()}` end,function(): Node
                        return if mode:peek()=="setup" then setup_fields() else cards()
                    end),
                    ui.show(function() return error_text:get()~="" or m.error:get()~="" end,ui.text {function() return if error_text:get()~="" then error_text:get() else m.error:get() end,color="$status.danger",wrap=true}),
                },
            },
            Dialog.Dialog {open=discard,title="Discard unsaved changes?",body="Your changes haven't been saved.",tone="danger",cancel="Keep editing",
                confirm={label="Discard",on_click=function() original:set("");open:set(false) end}},
        }
    end)
    return Editor
end
