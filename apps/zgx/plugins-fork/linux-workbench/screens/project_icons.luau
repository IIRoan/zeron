-- Local, profile-private artwork works without an account or an upload.
local Prefs=require("./prefs")
local icons=Prefs.persist("project_icons",{} :: any)
local serial=0
local M={error=ui.state("fork.project_icon.error",""),open=ui.state("fork.project_icon.open",false)}
local function failed(e: any) M.error:set(tostring(e));M.open:set(true) end
M.View=ui.component(function(): Node
    return require("@ds/dialog").Dialog {open=M.open,title="Couldn't set project image",body=function() return M.error:get() end,cancel="Close"}
end)
local function key(p: any): string return p.identity or p.id end
function M.of(p: any): string? return icons:get()[key(p)] end
function M.choose(p: any)
    serial+=1;local mine=serial;local captured=key(p)
    ui.open_files({title="Choose project image",filters={{"Images",{"png","jpg","jpeg","webp"}}}}):after(function(files: {any})
        if not files[1] or mine~=serial then return end
        ui.call("wallpaper.keep",{path=files[1].path}):after(function(image: any)
            if mine~=serial then return end
            local next=table.clone(icons:peek());next[captured]=image.path;icons:set(next)
        end,failed)
    end,failed)
end
function M.reset(p: any)
    serial+=1
    local next=table.clone(icons:peek());next[key(p)]=nil;icons:set(next)
end
return M
