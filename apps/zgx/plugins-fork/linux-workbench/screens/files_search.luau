-- Workspace search uses the same engine and explicit options as the GPUI fork.
local ds=require("@ds")
local route=require("@app/screens/shell/route")
local M={open=ui.state("fork.files.search_open",false),folder=ui.state("fork.files.search_folder","")}
function M.show(folder: string?) M.folder:set(folder or "");M.open:set(true) end
ui.command {id="fork.search.workspace",title="Find in Files",shortcut="cmd+shift+f",run=function() M.show() end}
M.View=ui.component(function(): Node
    local query=ui.use_state("");local include=ui.use_state("");local exclude=ui.use_state("")
    local match_case=ui.use_state(false);local whole=ui.use_state(false);local regex=ui.use_state(false);local ignored=ui.use_state(false)
    local delayed=ui.debounced(query,180)
    local result=ui.resource(function(): any
        if not M.open:get() then return nil end
        local c=route.session:get();local q=delayed:get()
        if not c or q=="" then return nil end
        return ui.call("files.search_content",{target={chat=c.id,space=c.project,device=c.device,root=c.cwd},query=q,limit=300,per_file=20,
            options={matchCase=match_case:get(),wholeWord=whole:get(),useRegex=regex:get(),include=include:get(),exclude=exclude:get(),folder=M.folder:get(),includeIgnored=ignored:get()}}):await()
    end)
    return ds.Dialog {open=M.open,title="Find in Files",size="lg",
        ui.column {w_full=true,gap="$space.3",
            ds.TextField {value=query,label="Search project contents",placeholder="Search project contents",w_full=true},
            ui.row {gap="$space.3",flex_wrap=true,
                ds.Checkbox {value=match_case,label="Match case"},ds.Checkbox {value=whole,label="Whole word"},
                ds.Checkbox {value=regex,label="Regular expression"},ds.Checkbox {value=ignored,label="Include ignored files"}},
            ui.row {w_full=true,gap="$space.2",
                ds.TextField {value=include,label="Files to include",placeholder="e.g. src/**, *.rs",grow=1,min_w=0},
                ds.TextField {value=exclude,label="Files to exclude",placeholder="e.g. tests/**",grow=1,min_w=0}},
            ds.TextField {value=M.folder,label="Search folder",placeholder="Project root",w_full=true},
            ui.presence(function(): string
                if not route.session:get() then return "no-chat" end
                if result.loading:get() then return "loading" end
                if result.error:get() then return "error" end
                local r=result.data:get();return if r then "matches" else "empty"
            end,function(kind: string): Node
                if kind=="loading" then return ui.text {"Searching…",color="$text.secondary"} end
                if kind=="error" then return ui.text {function() return tostring(result.error:get()) end,wrap=true,color="$status.danger"} end
                if kind=="no-chat" then return ui.text {"Open a chat to search its checkout.",color="$text.secondary"} end
                if kind=="empty" then return ui.text {"Search the current checkout. Results open at the matching line.",color="$text.secondary",wrap=true} end
                return ui.column {w_full=true,gap="$space.2",
                    ui.text {function() local r=result.data:get();return `{#((r and r.matches) or {})} matches{if r and r.truncated then " · showing first results" else ""}` end,color="$text.secondary"},
                    ui.scroll {id="fork-search-results",w_full=true,h=320,
                        ui.column {w_full=true,gap="$space.1",ui.each(ui.computed(function() local r=result.data:get();return (r and r.matches) or {} end),function(hit: any): Node
                            return ui.button {w_full=true,justify="start",p="$space.2",rounded="$radius.sm",hover={bg="$hover"},label=`{hit.path}:{hit.line}`,
                                on_click=function()
                                    local c=route.session:peek();if not c then return end
                                    M.open:set(false)
                                    require("@app/screens/panel").open_file(c,hit.path,nil,{line=hit.line,column=hit.column})
                                end,
                                ui.column {w_full=true,gap="$space.1",
                                    ui.text {`{hit.path}:{hit.line}`,text_size="$font.size.sm",color="$text.primary"},
                                    ui.text {hit.text or "",font="$font.mono",text_size="$font.size.xs",color="$text.secondary",wrap=true}},
                            }
                        end,{key=function(hit: any) return `{hit.path}:{hit.line}:{hit.column}` end})}},
                }
            end),
        },
    }
end)
return M
