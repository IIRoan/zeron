local Button=require("@ds/button")
local Dropdown=require("@ds/dropdown")
local Dialog=require("@ds/dialog").Dialog
local Checkbox=require("@ds/checkbox")
local unsaved=ui.shared("zeron.unsaved_editors",false)
return function(zeron: any,Model: any,Field: any,Toggle: any,ServiceEditor: any)
    return function(m: any): any
        local context=Model.context(nil,m.context.project)
        local w={selected=ui.use_state(""),section=ui.use_state("overview"),snapshots=ui.use_state({} :: any),drafts=ui.use_state({} :: any),
            originals={},epoch=ui.use_state(0),error=ui.use_state(""),busy=ui.use_state(false),remove=ui.use_state(false),force=ui.use_state(false),
            log_open=ui.use_state(false),log=ui.use_state(""),requests={}}
        local function params(key: string): any return if key=="" then {} else {checkoutPath=key} end
        local function call(method: string,p: any): any return zeron.workbench(method,context,p) end
        function w.fetch(key: string)
            w.requests[key]=(w.requests[key] or 0)+1;local serial=w.requests[key]
            call("GetWorktreeSettings",params(key)):after(function(snapshot: any)
                if serial~=w.requests[key] then return end
                local snapshots=table.clone(w.snapshots:peek());snapshots[key]=snapshot;w.snapshots:set(snapshots)
                if w.drafts:peek()[key]==nil then
                    local value={settings=Model.clone(snapshot.settings),databases=Model.clone(snapshot.databases or {})}
                    local drafts=table.clone(w.drafts:peek());drafts[key]=value;w.drafts:set(drafts);w.originals[key]=ui.json.encode(value);w.epoch:add(1)
                end
            end,function(e: any) w.error:set(Model.message(e)) end)
        end
        function w.load()
            w.snapshots:set({});w.drafts:set({});w.originals={};w.selected:set("");w.error:set("");w.fetch("")
        end
        ui.every(2000,function()
            if m.editor:peek() and w.selected:peek()~="" and w.drafts:peek()[w.selected:peek()] then w.fetch(w.selected:peek()) end
        end)
        function w.value(): any return w.drafts:peek() end
        function w.dirty(): boolean
            for key,value in w.drafts:peek() do if ui.json.encode(value)~=w.originals[key] then return true end end
            return false
        end
        function w.change(key: string,value: any)
            local page=w.selected:peek();local current=w.drafts:peek()[page];if not current then return end
            if ui.json.encode(current.settings[key])==ui.json.encode(value) then return end
            local drafts=Model.clone(w.drafts:peek());drafts[page].settings[key]=value;w.drafts:set(drafts)
        end
        local function database(id: string,key: string,value: any)
            local drafts=Model.clone(w.drafts:peek());local page=w.selected:peek()
            for _,s in drafts[page].databases do if s.id==id then
                if key=="readiness" then s.setupPrerequisite.command=value
                elseif key=="timeout" then s.setupPrerequisite.timeoutSeconds=tonumber(value) or 0 else s[key]=value end
            end end
            w.drafts:set(drafts)
        end
        function w.save()
            local key=w.selected:peek();local value=w.drafts:peek()[key];if not value or w.busy:peek() then return end
            local sent=Model.clone(value);local p=params(key);p.settings=sent.settings
            if key=="" then p.databases=sent.databases end
            w.busy:set(true);m.saving:set(true);w.error:set("")
            call("SaveWorktreeSettings",p):after(function()
                w.originals[key]=ui.json.encode(sent);w.busy:set(false);m.saving:set(false);w.fetch(key);m.refresh()
            end,function(e: any) w.busy:set(false);m.saving:set(false);w.error:set(Model.message(e)) end)
        end
        local function operation(method: string)
            local key=w.selected:peek();if key=="" or (w.busy:peek() and method~="CancelWorktreeSetup") then return end
            w.busy:set(true);w.error:set("")
            call(method,params(key)):after(function(result: any)
                w.busy:set(false);w.fetch(key)
                if method=="GetWorktreeSetupLog" then w.log:set(if type(result)=="string" then result else result.text or result.output or result.log or "");w.log_open:set(true) end
            end,function(e: any) w.busy:set(false);w.error:set(Model.message(e));w.fetch(key) end)
        end
        local function defaults()
            local key=w.selected:peek();if key=="" then return end
            local p=params(key);p.settings=ui.null;w.busy:set(true)
            call("SaveWorktreeSettings",p):after(function()
                local drafts=table.clone(w.drafts:peek());drafts[key]=nil;w.drafts:set(drafts);w.originals[key]=nil;w.busy:set(false);w.fetch(key)
            end,function(e: any) w.busy:set(false);w.error:set(Model.message(e)) end)
        end
        local function remove()
            local key=w.removing;if not key or key=="" then return end
            if unsaved:peek() then w.error:set("Save or close unsaved file editors before removing a worktree.");return end
            w.busy:set(true);w.error:set("")
            call("DeleteWorktree",{worktreePath=key,force=w.force:peek()}):after(function()
                w.busy:set(false);w.selected:set("");w.fetch("");w.epoch:add(1)
            end,function(e: any) w.busy:set(false);w.error:set(Model.message(e)) end)
        end
        local sections={{"overview","Overview"},{"branch","Branch names"},{"databases","Shared databases"},{"env","Environment files"},
            {"dependencies","Dependencies"},{"workflow","Setup workflow"},{"services","App services"},{"advanced","Advanced"}}
        local Mode=ui.component(function(p: any): Node
            local state=ui.use_state(p.value)
            ui.effect(function() p.changed(state:get()) end)
            return Dropdown {value=state,label=p.label,items=p.items,width=300}
        end)
        local function form(): Node
            local key=w.selected:get();local value=w.drafts:get()[key];local snapshot=w.snapshots:get()[key]
            if not value or not snapshot then return ui.text {"Loading project settings…",color="$text.secondary"} end
            local s=value.settings;local section=w.section:get();local children={}
            local function field(name: string,label: string,extra: any?)
                local p=extra or {};p.label=label;p.value=s[name];p.changed=function(v: any) w.change(name,if name=="commandTimeoutSeconds" then tonumber(v) or 0 else v) end
                table.insert(children,Field(p))
            end
            if section=="overview" then
                table.insert(children,ui.text {if key=="" then "Project defaults apply to every checkout without an override." else key,wrap=true,color="$text.secondary"})
                if key~="" then
                    local phase="Not prepared"
                    local phases={notPrepared="Not prepared",preparing="Preparing…",ready="Ready",failed="Failed",interrupted="Interrupted"}
                    for _,entry in snapshot.worktrees or {} do if entry.path==key then phase=(phases[entry.state.phase] or entry.state.phase) .. (if entry.state.error then `: {entry.state.error}` else "") end end
                    table.insert(children,ui.text {phase,wrap=true})
                    table.insert(children,ui.row {gap="$space.2",flex_wrap=true,
                        Button {"Prepare worktree",size="sm",disabled=w.busy,on_click=function() operation("PrepareWorktree") end},
                        Button {"Cancel setup",size="sm",tone="secondary",on_click=function() operation("CancelWorktreeSetup") end},
                        Button {"Show setup log",size="sm",tone="secondary",on_click=function() operation("GetWorktreeSetupLog") end}})
                    table.insert(children,Button {"Use project defaults",tone="secondary",on_click=defaults})
                    table.insert(children,Button {"Remove worktree…",tone="ghost",color="$status.danger",on_click=function() w.removing=key;w.force:set(false);w.remove:set(true) end})
                end
            elseif section=="branch" then field("branchPrefix","Branch prefix",{description="Used for new worktree branches and their automatic title rename."})
            elseif section=="env" then
                field("envProfile","Environment profile")
                table.insert(children,Button {"Add detected env files",size="sm",tone="secondary",on_click=function()
                    local list=Model.clone(s.envFiles or {});local seen={};for _,e in list do seen[e.path]=true end
                    for _,path in snapshot.detectedEnvFiles or {} do if not seen[path] then table.insert(list,{path=path,mode="follow"}) end end;w.change("envFiles",list);w.epoch:add(1)
                end})
                for i,e in s.envFiles or {} do
                    table.insert(children,ui.column {w_full=true,gap="$space.2",
                        Field {label="Environment file",value=e.path,changed=function(v: string) local list=Model.clone(w.drafts:peek()[key].settings.envFiles);list[i].path=v;w.change("envFiles",list) end},
                        Mode {label="Environment file handling",value=e.mode,items={{value="follow",label="Follow active checkout"},{value="copy",label="Copy once"},{value="link",label="Link"},{value="skip",label="Skip"}},
                            changed=function(v: string) local list=Model.clone(w.drafts:peek()[key].settings.envFiles);list[i].mode=v;w.change("envFiles",list) end},
                        Button {"Remove env file",tone="ghost",size="xs",on_click=function() local list=Model.clone(w.drafts:peek()[key].settings.envFiles);table.remove(list,i);w.change("envFiles",list);w.epoch:add(1) end}})
                end
                table.insert(children,Button {"Add env file",size="sm",tone="secondary",on_click=function() local list=Model.clone(s.envFiles or {});table.insert(list,{path=".env",mode="follow"});w.change("envFiles",list);w.epoch:add(1) end})
            elseif section=="dependencies" then
                table.insert(children,Mode {label="Dependencies",value=s.dependencies,items={{value="copy",label="Copy node_modules automatically"},{value="install",label="Install dependencies per worktree"},{value="link",label="Share dependency folders"},{value="skip",label="Let my setup command handle dependencies"}},changed=function(v: string) w.change("dependencies",v) end})
                field("installCommand","Install command",{description=snapshot.detectedInstallCommand or "Detected from the lockfile when empty."})
                table.insert(children,Field {label="Dependency paths (one per line)",multiline=true,value=table.concat(s.dependencyPaths or {},"\n"),changed=function(v: string) local paths={};for path in string.gmatch(v,"[^\r\n]+") do table.insert(paths,path) end;w.change("dependencyPaths",paths) end})
            elseif section=="workflow" then
                for _,item in {{"createCommand","Create worktree command"},{"setupCommand","Setup command"},{"activateCommand","Activate command"},{"cleanupCommand","Cleanup command"},{"removeCommand","Remove worktree command"}} do field(item[1],item[2],{multiline=true,code=true}) end
                table.insert(children,Toggle {label="Run setup commands in the main project folder",value=s.commandsInProject,changed=function(v: boolean) w.change("commandsInProject",v) end})
            elseif section=="advanced" then
                field("commandTimeoutSeconds","Command timeout (seconds)")
                table.insert(children,Mode {label="Service groups",value=s.serviceMode,items={{value="followActive",label="One active service group"},{value="parallel",label="A group per worktree"}},changed=function(v: string) w.change("serviceMode",v) end})
                local variables={};for k,v in s.variables or {} do table.insert(variables,`{k}={v}`) end;table.sort(variables)
                table.insert(children,Field {label="Variables (NAME=value, one per line)",multiline=true,value=table.concat(variables,"\n"),changed=function(v: string) local vars={};for line in string.gmatch(v,"[^\r\n]+") do local k,x=string.match(line,"^([^=]+)=(.*)$");if k then vars[k]=x end end;w.change("variables",vars) end})
            elseif section=="databases" then
                if key~="" then table.insert(children,ui.text {"Shared databases are configured on Project defaults.",color="$text.secondary",wrap=true})
                else
                    for _,db in value.databases do
                        table.insert(children,ui.column {w_full=true,gap="$space.2",
                            Field {label="Database name",value=db.name,changed=function(v: string) database(db.id,"name",v) end},
                            Field {label="Database command",value=db.command,changed=function(v: string) database(db.id,"command",v) end},
                            Field {label="Database directory",value=db.directory,changed=function(v: string) database(db.id,"directory",v) end},
                            Toggle {label="Restart database on failure",value=db.restartOnFailure,changed=function(v: boolean) database(db.id,"restartOnFailure",v) end},
                            Field {label="Readiness command",value=db.setupPrerequisite.command,changed=function(v: string) database(db.id,"readiness",v) end},
                            Field {label="Readiness timeout (seconds)",value=db.setupPrerequisite.timeoutSeconds,changed=function(v: string) database(db.id,"timeout",v) end},
                            Button {"Remove database",size="xs",tone="ghost",on_click=function() local d=Model.clone(w.drafts:peek());for i,x in d[key].databases do if x.id==db.id then table.remove(d[key].databases,i);break end end;w.drafts:set(d);w.epoch:add(1) end}})
                    end
                    table.insert(children,Button {"Add database",size="sm",on_click=function()
                        local d=Model.clone(w.drafts:peek());table.insert(d[key].databases,{id=`database-{os.time()}-{#d[key].databases+1}`,name="Database",command="",directory="",restartOnFailure=false,setupPrerequisite={command="",timeoutSeconds=120}});w.drafts:set(d);w.epoch:add(1)
                    end})
                end
            elseif section=="services" then
                local selected=Model.context(nil,m.context.project,if key=="" then nil else key)
                selected.key ..= ":worktree-service-editor"
                local services=Model.get(selected)
                table.insert(children,ui.text {"App services are saved separately from environment settings.",wrap=true,color="$text.secondary"})
                for _,service in snapshot.appServices or {} do table.insert(children,ui.text {`{service.name}: {service.command}`,wrap=true,font="$font.mono",text_size="$font.size.sm"}) end
                table.insert(children,Button {if key=="" then "Edit app services…" else "Customize for this worktree…",tone="secondary",on_click=function() services.snapshot:set(nil);services.editor:set(true) end})
                if key~="" then table.insert(children,Button {"Use project services",tone="ghost",disabled=w.busy,on_click=function()
                    w.busy:set(true)
                    call("SaveProjectTerminals",{checkoutPath=key,useProjectServices=true}):after(function()
                        w.busy:set(false);w.fetch(key);m.refresh()
                    end,function(e: any) w.busy:set(false);w.error:set(Model.message(e)) end)
                end}) end
                table.insert(children,ServiceEditor(services,function() w.fetch(key) end))
            end
            return ui.column {w_full=true,gap="$space.3",table.unpack(children)}
        end
        function w.View(): Node
            return ui.column {w_full=true,gap="$space.3",
                Dropdown {value=w.selected,label="Worktree settings page",width=440,items=function()
                    local list={{value="",label="Project defaults"}};local root=w.snapshots:get()[""]
                    for _,entry in (root and root.worktrees) or {} do if entry.path~=context.root then table.insert(list,{value=entry.path,label=`{entry.branch} — {entry.path}`}) end end;return list
                end,on_choose=function(key: string) w.section:set("overview");w.fetch(key);w.epoch:add(1) end},
                ui.show(function() return w.error:get()~="" end,ui.text {function() return w.error:get() end,wrap=true,color="$status.danger"}),
                ui.row {w_full=true,gap="$space.4",items="start",
                    ui.column {id="project-settings-nav",w=140,shrink=0,gap="$space.1",ui.each(sections,function(item: any): Node
                        return ui.button {role="tab",label=item[2],w_full=true,h=32,px="$space.2",items="center",justify="start",rounded="$radius.sm",
                            selected=function() return w.section:get()==item[1] end,
                            bg=function() return if w.section:get()==item[1] then "$zeron.secondary.bg" else "#00000000" end,
                            hover={bg="$hover"},on_click=function() w.section:set(item[1]) end,
                            ui.text {item[2],text_size="$font.size.sm"}}
                    end)},
                    ui.presence(function() return `{w.selected:get()}:{w.section:get()}:{w.epoch:get()}` end,function() return form() end,{grow=1,min_w=0})},
                Dialog {open=w.remove,title="Remove worktree?",tone="danger",body="Services stop first, then the saved cleanup workflow runs. The Git branch and archived conversation history are kept.",
                    confirm={label="Remove worktree",on_click=remove},Checkbox {value=w.force,label="Discard uncommitted changes"}},
                Dialog {open=w.log_open,title="Worktree setup log",size="lg",ui.scroll {w_full=true,h=360,ui.text {function() return w.log:get() end,font="$font.mono",wrap=true,text_size="$font.size.sm"}}},
            }
        end
        return w
    end
end
