local ds=require("@ds")
local route=require("@app/screens/shell/route")
local state=require("@app/screens/panel/state")
local Terminal=require("@app/screens/panel/terminal")
return ui.component(function(p: any): Node
    local asking=ui.use_state(false);local pending: any=nil
    local initialized={}
    local function key(): string local c=route.session:get();return `dock:{if c then c.id else ""}` end
    local function active(): any local s=state.of(key());return if s.active then state.find(key(),s.active) else nil end
    local function add() state.open(key(),"terminal") end
    ui.effect(function()
        local c=route.session:get();local k=key();local tabs=state.of(k).tabs
        if c and not initialized[k] and p.expanded:get() then
            initialized[k]=true
            if #tabs==0 then ui.after(0,function() if #state.of(k).tabs==0 then state.open(k,"terminal") end end) end
        end
    end)
    local function close()
        local tab=active();if not tab then return end
        local k=key()
        Terminal.closing(tab,function(busy: boolean)
            if busy then pending={key=k,tab=tab};asking:set(true)
            else Terminal.close(tab);state.close(k,tab.id) end
        end)
    end
    return ui.column {id="fork-terminal-dock",w_full=true,h_full=true,min_h=0,on_key=function(e: any): boolean
        if e.cmd and string.lower(e.key)=="w" then close();return true end;return false
    end,
        ui.row {id="fork-terminal-toolbar",w_full=true,h=36,shrink=0,pl="$space.2",pr=ui.raw(40,"room for the dock's hide control"),gap="$space.1",items="center",border_sides={0,0,1,0},border_color="$border.subtle",
            ui.text {"Terminals",text_size="$font.size.sm",weight="$font.weight.semibold"},
            ui.scroll_x {grow=1,min_w=0,h=30,
                ui.row {h_full=true,items="center",gap="$space.1",ui.each(ui.computed(function() return state.of(key()).tabs end),function(tab: any): Node
                    return ds.Button {`{tab.n or 1}`,label=`Terminal {tab.n or 1}`,size="xs",tone="ghost",on_click=function() state.activate(key(),tab.id) end}
                end,{key=function(tab: any) return tab.id end,direction="row"})}},
            ds.IconButton {icon="plus",label="New terminal",size="sm",on_click=add},
            ds.IconButton {icon="close",label="Close terminal",size="sm",on_click=close}},
        ui.show(p.expanded,ui.presence(function() local t=active();return if t then `{key()}:{t.id}` else "" end,function(which: string): Node
            local tab=active();if not tab then return ui.column {w_full=true,grow=1,items="center",justify="center",ds.Button {"New terminal",tone="secondary",on_click=add}} end
            return Terminal.Terminal {key=key(),tab=tab.id,session=route.session,project=route.project}
        end,{w_full=true,grow=1,min_h=0})),
        ds.Dialog {open=asking,title="Close running terminal?",body="The terminal still has a running process. Closing it stops that process.",tone="danger",confirm={label="Close terminal",on_click=function()
            if pending then Terminal.close(pending.tab);state.close(pending.key,pending.tab.id);pending=nil end
        end}},
    }
end)
