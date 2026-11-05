-- Engine-owned plugin storage keeps preferences when signed snapshot paths
-- change during upgrades. Return real states so normal controls still work.
local kept=ui.shared("plugin.linux-workbench.store",{} :: any)
local states: {[string]: any}={}
local M={}
function M.persist(key: string,default: any): any
    if states[key] then return states[key] end
    local initial=kept:peek()[key]
    local state=ui.state(`fork.kept.{key}`,if initial==nil then default else initial)
    states[key]=state
    local delayed=ui.debounced(state,160)
    ui.effect(function()
        local value=delayed:get()
        if ui.json.encode(value)~=ui.json.encode(kept:peek()[key]) then
            ui.invoke("plugin.linux-workbench.store_set",{key=key,value=if value==nil then ui.null else value})
        end
    end)
    return state
end
return M
