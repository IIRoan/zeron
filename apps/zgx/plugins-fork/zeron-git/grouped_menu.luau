local ContextMenu=require("./ui/context_menu")
local IconButton=require("@ds/icon_button").IconButton
local commands=require("./command_items")
local Branches=require("./branches")
local order={"Fetch, pull and push","Branches","Stashes","Remotes","History","Changes","Commit"}
return function(m: any): Node
    local anchor: any=nil;local show: any=nil
    return ContextMenu {width=240,hook=function(open: any) show=open end,items=function(): {any}
        local pin=m.workbench.capture();local groups={}
        for _,name in order do
            local entries={}
            for _,item in commands do if item[3]==name then
                table.insert(entries,{label=item[2],disabled=m.workbench.busy:peek(),on_select=function()
                    local k=item[1]
                    if k=="switchBranch" or k=="pullFrom" or k=="merge" or k=="rebase" or k=="deleteBranch" then Branches.open(m,k,pin)
                    else m.set_scope("working",nil);m.workbench.command_request:set({kind=k,pin=pin}) end
                end})
            end end
            table.insert(groups,{label=name,submenu=entries})
        end
        return groups
    end,
        IconButton {id="git-more",icon="more-horizontal",label="More",tone="quiet",on_mount=function(el: any) anchor=el end,
            on_click=function() if show and anchor then show(anchor.x,anchor.y+anchor.height) end end}}
end
