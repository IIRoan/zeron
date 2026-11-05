local ds={Button=require("@ds/button"),Dialog=require("@ds/dialog").Dialog,TextField=require("@ds/text_field"),
    Icon=require("@ds/icon"),FadeText=require("@ds/fade_text")}
local choices=require("./branch_choices")
local titles={switchBranch="Checkout to…",pullFrom="Pull from…",merge="Merge branch…",rebase="Rebase onto…",deleteBranch="Delete branch…"}
local M={}
local function controller(m: any): any
    if m.workbench.branch_picker then return m.workbench.branch_picker end
    local p={open=ui.state(`git.{m.id}.branch_picker.open`,false),kind=ui.state(`git.{m.id}.branch_picker.kind`,"switchBranch"),
        query=ui.state(`git.{m.id}.branch_picker.query`,""),all=ui.state(`git.{m.id}.branch_picker.all`,false),
        branches=ui.state(`git.{m.id}.branch_picker.branches`,{} :: any),loading=ui.state(`git.{m.id}.branch_picker.loading`,false),
        error=ui.state(`git.{m.id}.branch_picker.error`,""),active=ui.state(`git.{m.id}.branch_picker.active`,1),
        confirm=ui.state(`git.{m.id}.branch_picker.confirm`,false),serial=0,pin=nil,pending=nil}
    m.workbench.branch_picker=p;return p
end
function M.open(m: any,kind: string,pin: any?)
    local p=controller(m);p.serial+=1;local mine=p.serial
    p.kind:set(kind);p.query:set("");p.all:set(false);p.error:set("");p.loading:set(true);p.branches:set({});p.pin=pin or m.workbench.capture();p.open:set(true)
    m.workbench.call("GetCheckoutBranches",{},p.pin):after(function(result: any)
        if mine~=p.serial then return end;p.branches:set(result.branches or result);p.loading:set(false)
    end,function(e: any) if mine==p.serial then p.loading:set(false);p.error:set(tostring(e)) end end)
end
function M.Button(m: any): Node
    return ds.Button {function() local s=m.status:get();return (s and (s.branch or (s.oid and string.sub(s.oid,1,7)))) or "Branch" end,
        id="git-branch",label="Branch",size="xs",tone="ghost",disabled=function() return m.workbench.busy:get() end,
        on_click=function() M.open(m,"switchBranch") end}
end
M.View=ui.component(function(p: any): Node
    local m=p.model;local c=controller(m)
    local rows=ui.computed(function() return choices.list(c.branches:get(),c.kind:get(),c.query:get(),c.all:get(),os.time()) end)
    ui.effect(function() rows:get();c.active:set(1) end)
    local function choose(b: any?,create: boolean?)
        if c.loading:peek() or c.error:peek()~="" then return end
        local kind=c.kind:peek();local action
        if create then action={kind="createBranch",name=string.match(c.query:peek(),"^%s*(.-)%s*$")}
        elseif not b then return
        elseif b.current and kind=="switchBranch" then c.open:set(false);return
        else action={kind=kind};if kind=="deleteBranch" then action.name=choices.ref(b) else action.branch=choices.ref(b) end end
        c.open:set(false)
        if create or kind=="deleteBranch" then c.pending=action;c.confirm:set(true)
        else m.workbench.action(action,c.pin) end
    end
    return ui.div {
        ds.Dialog {open=c.open,title=function() return titles[c.kind:get()] end,size="md",
            ui.column {w_full=true,gap="$space.2",on_key=function(e: any): boolean
                local list=rows:peek()
                if e.key=="ArrowDown" or e.key=="ArrowUp" then c.active:set(math.clamp(c.active:peek()+(if e.key=="ArrowDown" then 1 else -1),1,math.max(1,#list)));return true end
                if e.key=="Enter" then choose(list[c.active:peek()],#list==0 and c.kind:peek()=="switchBranch" and string.find(c.query:peek(),"%S")~=nil);return true end
                return false
            end,
                ds.TextField {value=c.query,label="Search all branches",placeholder="Search all branches",w_full=true},
                ui.row {w_full=true,items="center",gap="$space.2",
                    ui.text {function() return if c.all:get() or c.query:get()~="" then "All matching branches" else "Recent branches" end,grow=1,color="$text.secondary"},
                    ds.Button {"Show all branches",size="xs",tone="ghost",on_click=function() c.all:set(true) end}},
                ui.show(c.loading,ui.text {"Loading branches…",color="$text.secondary"}),
                ui.show(function() return c.error:get()~="" end,ui.text {function() return c.error:get() end,wrap=true,color="$status.danger"}),
                ui.scroll {id="git-branch-picker-list",w_full=true,h=280,
                    ui.column {w_full=true,gap="$space.1",ui.each(rows,function(b: any,index: number): Node
                        return ui.button {w_full=true,h=34,px="$space.2",gap="$space.2",items="center",label=`{b.name}{if b.remote then " (remote)" else ""}`,
                            bg=function() return if c.active:get()==index then "$hover" else "#00000000" end,
                            on_hover=function(over: boolean) if over then c.active:set(index) end end,on_click=function() choose(b) end,
                            ds.Icon {name=if b.remote then "cloud" else "git-branch",size=14},
                            ds.FadeText {b.name,grow=1,min_w=0},if b.current then ui.text {"Current",color="$text.tertiary",text_size="$font.size.xs"} else nil}
                    end,{key=choices.ref})}},
                ui.show(function() return not c.loading:get() and c.error:get()=="" and #rows:get()==0 end,
                    ui.column {w_full=true,gap="$space.2",
                        ui.text {"No matching branches. Search or show all branches.",color="$text.secondary",wrap=true},
                        ui.show(function() return c.kind:get()=="switchBranch" and string.find(c.query:get(),"%S")~=nil end,
                            ds.Button {function() return `Create branch ‘{c.query:get()}’…` end,on_click=function() choose(nil,true) end})}),
            },
        },
        ui.presence(function() return if c.confirm:get() and c.pending then c.pending.kind else nil end,function(kind: string): Node
            return ds.Dialog {open=c.confirm,title=function() return if kind=="createBranch" then `Create branch {c.pending.name}?` else `Delete branch {c.pending.name}?` end,
                tone=if kind=="deleteBranch" then "danger" else nil,
                body=if kind=="createBranch" then "Create this branch from the current checkout and switch to it." else "Delete this local branch? Git will refuse if it contains unmerged commits.",
                confirm={label="Confirm",on_click=function() m.workbench.action(c.pending,c.pin) end}}
        end),
    }
end)
return M
