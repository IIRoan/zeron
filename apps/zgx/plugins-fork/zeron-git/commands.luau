--!strict
local Button=require("@ds/button")
local Dropdown=require("@ds/dropdown")
local TextField=require("@ds/text_field")
local Dialog=require("@ds/dialog").Dialog
local Branches=require("./branches")
local COMMANDS=require("./command_items")
return function(zeron: any)
    return ui.component(function(p: any): Node
        local m,w=p.model,p.model.workbench
        local command=ui.use_state("");local prompt=ui.use_state(false);local title=ui.use_state("")
        local choice=ui.use_state("");local extra=ui.use_state("");local kind=ui.use_state("")
        local pin: any=nil
        local function commit(amend: boolean,follow: string?) w.commit(amend,follow,pin) end
        local function run()
            local k=kind:peek();local v=choice:peek();local x=extra:peek()
            local action: any={kind=k}
            if k=="createBranch" then action.name=v;action.start=if x~="" then x else nil
            elseif k=="renameBranch" or k=="deleteBranch" then action.name=v
            elseif k=="switchBranch" or k=="pullFrom" or k=="merge" or k=="rebase" then action.branch=v
            elseif k=="applyStash" or k=="popStash" or k=="dropStash" or k=="restoreStash" then
                action={kind=if k=="restoreStash" then "applyStash" else k,sha=v,reinstateStaged=k=="restoreStash"}
            elseif k=="cherryPick" or k=="revertCommit" then action.sha=v
            elseif k=="addRemote" then action.name=v;action.url=x
            elseif k=="removeRemote" then action.name=v
            elseif k=="publish" then action.remote=v
            elseif k=="stash" or k=="stashUntracked" then action={kind="stash",message=v,includeUntracked=k=="stashUntracked"}
            end
            prompt:set(false)
            w.action(action,pin,function(result: any) if result.commitMessage then m.message:set(result.commitMessage) end end)
        end
        local function choose(k: string)
            kind:set(k);choice:set("");extra:set("")
            for _,c in COMMANDS do if c[1]==k then title:set(c[2]) end end
            if not pin then pin=w.capture() end
            if k=="switchBranch" or k=="pullFrom" or k=="merge" or k=="rebase" or k=="deleteBranch" then Branches.open(m,k,pin);return end
            if k=="refresh" then w.refresh();m.refresh(true);return
            elseif k=="undoDiscard" then w.undo();return
            elseif k=="stageAll" or k=="unstageAll" then p.stage_all(k=="stageAll");return
            elseif k=="discardAll" or k=="discardEverything" then p.discard_all(k=="discardEverything",pin);return
            elseif string.sub(k,1,4)=="sort" then p.sort:set(string.lower(string.sub(k,5)));return
            elseif k=="amend" then w.toggle_amend();return
            elseif k=="commit" or k=="commitPush" or k=="commitSync" then commit(w.amend:peek(),if k=="commitPush" then "push" elseif k=="commitSync" then "sync" else nil);return
            elseif k=="fetch" or k=="pull" or k=="push" or k=="sync" or k=="continue" or k=="pullMerge" or k=="pullRebase" then
                w.action(if k=="pullMerge" or k=="pullRebase" then {kind="pull",rebase=k=="pullRebase"} else {kind=k},pin);return
            end
            w.load_details()
            if k=="applyLatestStash" or k=="popLatestStash" then
                local captured=pin
                w.call("GetCheckoutGitDetails",{},captured):after(function(d: any)
                    local stash=d.stashes[1]
                    if stash then w.action({kind=if k=="applyLatestStash" then "applyStash" else "popStash",sha=stash.sha,reinstateStaged=false},captured)
                    else w.error:set("No stash is available.") end
                end,function(e: any) w.error:set(tostring(e)) end);return
            end
            prompt:set(true)
        end
        ui.effect(function()
            local requested=w.command_request:get()
            if requested then ui.after(0,function()
                if w.command_request:peek()~=requested then return end
                w.command_request:set(nil);pin=requested.pin;choose(requested.kind)
            end) end
        end)
        local function choices(): {any}
            local d=w.details:get();if not d then return {} end
            local k=kind:get();local list={}
            if k=="incoming" or k=="outgoing" or k=="recent" or k=="cherryPick" or k=="revertCommit" then
                local commits=if k=="cherryPick" or k=="revertCommit" then d.recent else d[k]
                for _,c in commits or {} do table.insert(list,{value=c.sha,label=`{string.sub(c.sha,1,8)} {c.subject}`}) end
            elseif string.find(string.lower(k),"stash",1,true) then for _,s in d.stashes do table.insert(list,{value=s.sha,label=`{s.selector} {s.subject}`}) end
            elseif k=="removeRemote" or k=="publish" then for _,r in d.remotes do table.insert(list,{value=r.name,label=`{r.name} — {r.url}`}) end
            else for _,b in d.branches do table.insert(list,{value=b.name,label=b.name}) end end
            return list
        end
        return ui.row {items="center",gap="$space.1",
            Dropdown {value=command,label="Git commands",placeholder="Git commands…",search="Search Git commands",width=360,
                items=function() local items={};for _,c in COMMANDS do table.insert(items,{value=c[1],label=c[2]}) end;return items end,
                on_open=function() pin=w.capture();w.load_details() end,
                on_choose=function(k: string) choose(k);ui.after(0,function() command:set("") end) end},
            ui.presence(function() return kind:get() end,function(k: string): Node return Dialog {open=prompt,title=function() return title:get() end,size="md",
                tone=if k=="dropStash" or k=="deleteBranch" or k=="undoCommit" or k=="abort" or k=="revertCommit" then "danger" else "default",
                confirm=if k=="incoming" or k=="outgoing" or k=="recent" then nil else {label="Run",on_click=run},
                ui.text {"Applies to the checkout and branch shown when this command was opened.",wrap=true,color="$text.secondary"},
                Dropdown {value=choice,items=choices,placeholder="Choose a branch, stash, remote or commit",search="Search",width=420,
                    on_choose=function(sha: string)
                        if k=="incoming" or k=="outgoing" or k=="recent" then
                            local d=w.details:peek();for _,c in (d and d[k]) or {} do
                                if c.sha==sha then prompt:set(false);p.open_commit(c);break end
                            end
                        end
                    end},
                TextField {value=choice,label="Name, branch or commit",placeholder="Name, branch or commit",w_full=true},
                TextField {value=extra,label="Start point or remote URL",placeholder="Start point (create branch) or URL (add remote)",w_full=true},
            } end),
        }
    end)
end
