--!strict
local Button=require("@ds/button")
local Dropdown=require("@ds/dropdown")
local TextArea=require("@ds/text_area")
local ContextMenu=require("./ui/context_menu")
local Dialog=require("@ds/dialog").Dialog
local Changes=require("./ui/git_changes")
local FadeText=require("@ds/fade_text")
local git=require("./ui/git")
local Icon=require("@ds/icon")
local IconButton=require("@ds/icon_button").IconButton
local Selection=require("./selection")
local fm=require("./fork_motion")
local conflict=Selection.conflict
-- Past this many rows the list is virtual (`ui.list`: rows built as they
-- show) and its rows don't slide. ponytail: one cap; a virtual list that
-- animates moves would lift it.
local MANY=400
local function status(row: any,repository: any): (string,string)
    if row.group=="conflicts" then return "!","$danger" end
    local value=if row.group=="staged" then row.file.index else row.file.worktree
    local letters={added="A",modified="M",deleted="D",renamed="R",copied="C",untracked="U",typeChanged="T"}
    local letter=letters[value] or "!"
    for _,path in (repository and repository.submodules) or {} do if path==row.file.path then letter="S" end end
    local color=if value=="added" or value=="untracked" then "$success" elseif value=="deleted" then "$danger"
        elseif value=="renamed" or value=="copied" then "$accent" else "$warning"
    return letter,color
end
local function patch_lines(text: string): {any}
    local lines={};local started=false
    for line in string.gmatch(text .. "\n","([^\n]*)\n") do
        if string.sub(line,1,2)=="@@" then started=true end
        if started then table.insert(lines,line) end
    end
    if lines[#lines]=="" then table.remove(lines) end
    return git.parse(lines,10000)
end
return function(zeron: any,Repo: any)
    local Commands=require("./commands")(zeron)
    return ui.component(function(p: any): Node
        local m,w=p.model,p.model.workbench
        w.watch_draft()
        local sort=ui.persist("git.fork.sort","path")
        local folded=ui.persist("git.fork.group_folded",{} :: any)
        local selected,chosen,anchor=w.selected,w.chosen,w.anchor
        local diff=ui.use_state(nil :: any)
        local discard=ui.use_state(false);local preview: any=nil;local discard_pin: any=nil
        local list_offset=ui.use_state(0);local diff_offset=ui.use_state(0)
        local box=ui.ref();local list_box=ui.ref()
        local focus_inside=false;local hover_inside=false
        ui.on_cleanup(function() selected:set({});chosen:set(nil);anchor:set(nil) end)
        local commenting=ui.use_state(false);local note=ui.use_state("");local comment: any=nil
        local commit_menu: any=nil;local commit_anchor: any=nil
        local branch_info=ui.use_state(nil :: any)
        ui.effect(function()
            local s=m.status:get();local device,cwd=m.where()
            branch_info:set(if s then {session=m.id,device=device,cwd=cwd,branch=s.branch,upstream=s.upstream,
                remote=s.remote,remote_url=s.remote_url,pushed=s.pushed,ahead=s.ahead,behind=s.behind,base=s.base,base_ahead=s.base_ahead,head=s.oid} else nil)
        end)
        local paint_status=ui.state(`git.{m.id}.highlight_status`,"idle")
        local diff_cache: any=nil;local diff_version=ui.use_state(0);local last=""
        ui.after(0,function() w.refresh();w.load_details() end)
        local previous_repository=w.repository:peek()
        ui.effect(function()
            local repository=w.repository:get()
            if repository==previous_repository then return end
            previous_repository=repository
            ui.after(0,function() selected:set({});chosen:set(nil);anchor:set(nil);w.details:set(nil);m.refresh(true);w.load_details() end)
        end)
        ui.every(3000,function() if zeron.window().shown and (box.width or 0)>0 and (box.height or 0)>0 then w.refresh() end end)
        local function rows(): {any}
            local r=w.repo();local groups={conflicts={},staged={},unstaged={}}
            for _,f in (r and r.files) or {} do
                if conflict(f) then table.insert(groups.conflicts,f)
                else
                    if f.index~="unchanged" and f.index~="untracked" then table.insert(groups.staged,f) end
                    if f.worktree~="unchanged" or f.index=="untracked" then table.insert(groups.unstaged,f) end
                end
            end
            -- (`id`, the row's in the list: a file's path where it first
            -- shows, so staging or unstaging it moves its row to its new
            -- group; a file in both groups is twice there, the second by
            -- group.)
            local out={};local seen={}
            for _,group in {"conflicts","staged","unstaged"} do
                local files=groups[group]
                table.sort(files,function(a: any,b: any)
                    local by=sort:get();local aa=if by=="name" then string.match(a.path,"[^/]+$") elseif by=="status" then a.index .. a.worktree else a.path
                    local bb=if by=="name" then string.match(b.path,"[^/]+$") elseif by=="status" then b.index .. b.worktree else b.path
                    return if aa==bb then a.path<b.path else aa<bb
                end)
                if #files>0 then table.insert(out,{key=group,id=`#{group}`,group=group,title=`{string.upper(string.sub(group,1,1))}{string.sub(group,2)} ({#files})`}) end
                if not folded:get()[`{w.repository:get()}:{group}`] then
                    for _,f in files do
                        table.insert(out,{key=`{group}:{f.path}`,id=if seen[f.path] then `{group}:{f.path}` else f.path,group=group,file=f})
                        seen[f.path]=true
                    end
                end
            end
            return out
        end
        local row_list=ui.computed(rows)
        -- A diff always shows while there are changes: the chosen file's
        -- (in its new group once it's staged or unstaged), else the first.
        ui.effect(function()
            local list=row_list:get();local now=chosen:peek();local first,same=nil,nil
            for _,row in list do
                if row.file then
                    first=first or row
                    if now and row.key==now.key then return end
                    if now and not same and row.file.path==now.file.path then same=row end
                end
            end
            local want=same or first
            if want~=now then ui.after(0,function() chosen:set(want);diff_offset:set(0) end) end
        end)
        local function paths(group: string?,all: boolean?): {string}
            return Selection.paths(w.repo(),selected:get(),group,all)
        end
        local function ask(paths: {string},include_staged: boolean,pin: any?)
            if #paths==0 then return end
            local captured=pin or w.capture()
            w.discard(paths,include_staged,captured,function(value: any) preview=value;discard_pin=captured;discard:set(true) end)
        end
        local function stage_paths(targets: {string},on: boolean)
            -- Keep keyboard focus on the stable list. The clicked file
            -- moves rows and its Stage button disables during the write.
            local list=list_box:peek();if list then list:focus({scroll=false}) end
            w.stage(targets,on)
            ui.after(0,function()
                local mounted=list_box:peek();if mounted then mounted:focus({scroll=false}) end
            end)
        end
        local function stage_all(on: boolean) stage_paths(paths(if on then "unstaged" else "staged",true),on) end
        -- What the tab's ⋯ menu has of this view's own, before Git's groups.
        w.menu_extra=function(): {any}
            return {
                {label="Stage selected",disabled=not w.can_stage(paths("unstaged",false)) and not w.can_stage(paths("conflicts",false)),
                    on_select=function() local targets=paths("unstaged",false);for _,path in paths("conflicts",false) do table.insert(targets,path) end;stage_paths(targets,true) end},
                {label="Unstage selected",disabled=not w.can_stage(paths("staged",false)),on_select=function() stage_paths(paths("staged",false),false) end},
                {label="Stage all",disabled=not w.can_stage(paths("unstaged",true)),on_select=function() stage_all(true) end},
                {label="Unstage all",disabled=not w.can_stage(paths("staged",true)),on_select=function() stage_all(false) end},
                {label="Undo discard",disabled=w.recovery:peek()==nil or w.busy:peek(),on_select=w.undo},
                {separator=true},
                {label="Side by side",checked=function() return git.prefs.split:get() end,keep_open=true,on_select=function() git.prefs.split:set(not git.prefs.split:peek()) end},
            }
        end
        ui.on_cleanup(function() w.menu_extra=nil end)
        ui.effect(function()
            if m.discard_ask:get() then ui.after(0,function() m.discard_ask:set(nil);ask(paths(nil,true),true) end) end
        end)
        -- Each file row's button, by its key: the chosen one is kept in view.
        local row_els: {[string]: any}=setmetatable({},{__mode="v"}) :: any
        local function select(row: any,event: any?)
            local next,next_anchor=Selection.choose(selected:peek(),anchor:peek(),row_list:peek(),row,event or {})
            selected:set(next);anchor:set(next_anchor);chosen:set(row);diff_offset:set(0)
            local index=0;for i,r in row_list:peek() do if r.key==row.key then index=i end end
            local list=list_box:peek()
            -- (A virtual list scrolls to the row; otherwise the row, taking
            -- focus, is scrolled to.)
            if list and not list:scroll_to_row(index,{align="nearest"}) then
                local el=row_els[row.key];if el then el:focus() end
            end
        end
        ui.effect(function()
            local row=chosen:get();w.revision:get();local repo=w.repository:get()
            if not row then last="";diff:set(nil);diff_cache=nil;return end
            if w.staging_pending(row.file.path)~=nil then last="";diff:set(nil);diff_cache=nil;diff_version:add(1);return end
            local key=`{repo}:{row.key}:{w.revision:peek()}`;last=key
            w.call("GetCheckoutChangeDiff",{path=row.file.path,staged=row.group=="staged"}):after(function(result: any)
                if last~=key then return end
                diff_cache=patch_lines(result.patch or "");diff:set({truncated=result.truncated,checksum=result.checksum});diff_version:add(1)
                -- Optional full texts come from the same captured tree pair.
                -- A racing change leaves the engine patch intact and falls
                -- back to independent old/new line lexers.
                local context=w.context();local cwd=context.scope.cwd .. (if repo~="" then "/" .. repo else "")
                paint_status:set("reading sources")
                zeron.service(context.scope.device):call("selection_diff",{cwd=cwd,path=row.file.path,old=row.file.oldPath,staged=row.group=="staged"}):after(function(source: any)
                    if last~=key or not source.oldText or not source.newText then paint_status:set("stale or binary");return end
                    local parsed=patch_lines(source.text or "")
                    if ui.json.encode(parsed)~=ui.json.encode(diff_cache) then paint_status:set("snapshot changed");return end
                    local old,new,pairs={},{},{}
                    for _,line in parsed do if line.old then table.insert(old,line.old) end;if line.new then table.insert(new,line.new) end end
                    for _,pair in git.pairs(parsed) do if pair.left and pair.right and pair.left.kind=="-" and pair.right.kind=="+" then table.insert(pairs,{pair.left.old,pair.right.new}) end end
                    paint_status:set("highlighting")
                    zeron.workbench("HighlightGitDiff",context,{path=row.file.path,oldText=source.oldText,newText=source.newText,oldLines=old,newLines=new,pairs=pairs}):after(function(paint: any)
                        if last==key then git.paint(diff_cache,paint);diff_version:add(1);paint_status:set("ready") end
                    end,function(e: any) paint_status:set(tostring(e));print(`git highlighting: {e}`) end)
                end,function(e: any) paint_status:set(tostring(e));print(`git diff sources: {e}`) end)
            end,function(e: any) if last==key then w.error:set(tostring(e)) end end)
        end)
        local function menu(row: any): {any}
            if not selected:peek()[row.key] then select(row,{}) end
            local targets=Selection.paths(w.repo(),selected:peek(),row.group,false,row)
            local pin=w.capture()
            local function copy_paths(absolute: boolean)
                local copied={}
                for _,path in Selection.paths(w.repo(),selected:peek(),nil,false,row) do
                    local relative=(if pin.params.repository~="" then pin.params.repository .. "/" else "") .. path
                    table.insert(copied,if absolute then pin.context.scope.cwd .. "/" .. relative else relative)
                end
                ui.clipboard.write(table.concat(copied,"\n"))
            end
            return {
                {label="Open changes",disabled=w.staging_pending(row.file.path)~=nil,on_select=function() chosen:set(row) end},
                {separator=true},
                {label=if row.group=="staged" then "Unstage selected" else "Stage selected",disabled=not w.can_stage(targets),on_select=function() stage_paths(targets,row.group~="staged") end},
                {label="Discard selected…",disabled=row.group=="conflicts" or w.busy:peek(),on_select=function() ask(targets,row.group=="staged",pin) end},
                {label="Resolve with current",disabled=row.group~="conflicts",on_select=function() w.action({kind="resolveConflict",path=row.file.path,incoming=false},pin) end},
                {label="Resolve with incoming",disabled=row.group~="conflicts",on_select=function() w.action({kind="resolveConflict",path=row.file.path,incoming=true},pin) end},
                {separator=true},
                {label="Copy relative path",on_select=function() copy_paths(false) end},
                {label="Copy path",on_select=function() copy_paths(true) end},
            }
        end
        local function list_key(e: any): boolean
            if e.editing then return false end
            local key=string.lower(e.key)
            if key=="escape" then selected:set({});chosen:set(nil);anchor:set(nil);return true end
            if key=="a" and (e.ctrl or e.meta) then
                local next={};for _,row in row_list:peek() do if row.file then next[row.key]=true end end
                selected:set(next);return true
            end
            if key=="delete" or key=="backspace" then
                local row=chosen:peek()
                if row and row.group=="unstaged" and not w.busy:peek() then ask(paths("unstaged",false),false) end
                return true
            end
            if key~="arrowup" and key~="arrowdown" and key~="home" and key~="end" then return false end
            local files={};local current=0
            for _,row in row_list:peek() do
                if row.file then
                    table.insert(files,row)
                    if chosen:peek() and chosen:peek().key==row.key then current=#files end
                end
            end
            if #files==0 then return true end
            local index=if key=="home" then 1 elseif key=="end" then #files
                elseif key=="arrowup" then math.max(1,current-1) else math.min(#files,current+1)
            select(files[index],e);return true
        end
        return ui.column {id="git-fork-changes",ref=box,w_full=true,grow=1,min_h=0,gap="$space.2",
            on_focus=function() focus_inside=true end,
            on_hover=function(inside: boolean) hover_inside=inside end,
            on_blur=function()
                focus_inside=false
                ui.after(0,function()
                    -- A successful action disables its own button and can
                    -- blur it. That is still an interaction inside this
                    -- surface, so keep the selection on the moved files.
                    if not focus_inside and not hover_inside and zeron.window().active and not discard:peek() and not w.conflict_open:peek() then
                        selected:set({});anchor:set(nil)
                    end
                end)
            end,
            -- (Runs what the ⋯ menu asks, and asks what a command needs.)
            Commands {model=m,sort=sort,stage_all=stage_all,discard_all=function(all: boolean,pin: any) ask(paths(nil,true),all,pin) end,
                open_commit=function(c: any) if p.panel then Repo.open_commit(p.panel,m.id,c) end end},
            -- (Which repository, where the checkout holds more than one.)
            fm.show(function() return #w.repositories:get()>1 end,Dropdown {value=w.repository,label="Repository",items=function() local items={};for _,r in w.repositories:get() do table.insert(items,{value=r.path,label=r.name .. (if r.complete then "" else " (incomplete)")}) end;return items end}),
            -- The commit, first: its message (Ctrl+Enter commits) and the
            -- button, its ways (push, sync, amend) in the menu beside it.
            -- (One line: the message growing, Commit and its menu at its right.)
            ui.row {id="git-commit-box",w_full=true,gap="$space.1",items="start",
                ui.div {grow=1,min_w=0,
                    TextArea {value=m.message,name="Commit message",placeholder="Message (Ctrl+Enter to commit)",w_full=true,rows=1,max_rows=6,
                        on_submit=function() if w.commit_reason()==nil then w.commit(w.amend:peek()) end end}},
                ui.row {shrink=0,gap=ui.raw(2,"Commit and its menu, one control"),items="center",
                    Button {function() return if w.busy:get() then "Working…" elseif w.amend:get() then "Amend" else "Commit" end,
                        disabled=function() return w.commit_reason()~=nil end,tooltip=ui.text {function() return w.commit_reason() or "Commit the staged changes" end},
                        on_click=function() w.commit(w.amend:peek()) end},
                    ContextMenu {width=220,hook=function(open: any) commit_menu=open end,items=function(): {any}
                        local pin=w.capture()
                        local function run(kind: string) return function() w.command_request:set({kind=kind,pin=pin}) end end
                        return {
                            {label="Commit and push",disabled=w.commit_reason()~=nil,on_select=run("commitPush")},
                            {label="Commit and sync",disabled=w.commit_reason()~=nil,on_select=run("commitSync")},
                            {separator=true},
                            {label="Amend last commit",checked=function() return w.amend:get() end,keep_open=true,on_select=function() w.toggle_amend() end},
                            {label="Undo last local commit",disabled=w.busy:peek(),on_select=run("undoCommit")},
                        }
                    end,
                        IconButton {icon="alt-arrow-down",label="More ways to commit",size="sm",tone="normal",on_mount=function(el: any) commit_anchor=el end,
                            on_click=function() if commit_menu and commit_anchor then commit_menu(commit_anchor.x,commit_anchor.y+commit_anchor.height) end end}},
                },
            },
            ui.row {class="git-branch-actions",w_full=true,provide={git_branch=branch_info},
                zeron.slot("branch",{direction="row",gap="$space.1",items="center"})},
            fm.show(function() return w.error:get()~="" end,ui.text {function() return w.error:get() end,color="$danger",wrap=true}),
            fm.show(function() local r=w.repo();return r~=nil and not r.complete end,ui.text {"Repository status is incomplete. Refresh before changing files.",color="$warning",wrap=true}),
            -- A clean tree says so, rather than an empty list and diff (the
            -- one fading into the other).
            ui.presence(function(): string?
                local n=#row_list:get()
                if n==0 then return if w.repo()~=nil then "clean" else nil end
                return if n>MANY then "many" else "few"
            end,function(kind: string): Node
                if kind=="clean" then return ui.column {id="git-fork-clean",w_full=true,grow=1,min_h=0,items="center",justify="center",gap="$space.1",
                    ui.text {"No changes",weight="$font.weight.semibold",color="$text.secondary"},
                    ui.text {function() local r=w.repo();local s=r and r.git;return if s and s.branch then `{s.branch} matches its last commit.` else "The working tree matches its last commit." end,text_size="$font.size.sm",color="$text.tertiary"},
                } end
                local function file_row(row: any): Node
                        if not row.file then return ui.row {w_full=true,h=32,items="center",
                            ui.button {label=row.title,grow=1,h_full=true,justify="start",on_click=function()
                                local key=`{w.repository:peek()}:{row.group}`;local next=table.clone(folded:peek());next[key]=not next[key];folded:set(next)
                            end,
                                Icon {name=function() return if folded:get()[`{w.repository:get()}:{row.group}`] then "alt-arrow-right" else "alt-arrow-down" end,size=12},
                                ui.text {row.title,weight="$font.weight.semibold"}},
                            IconButton {icon=if row.group=="staged" then "minus" else "plus",label=if row.group=="staged" then "Unstage group" else "Stage group",size="sm",
                                disabled=function() return not w.can_stage(paths(row.group,true)) end,on_click=function() stage_paths(paths(row.group,true),row.group~="staged") end},
                        } end
                        local letter,color=status(row,w.repo())
                        return ContextMenu {items=function() return menu(row) end,
                            ui.row {class="git-file-row",w_full=true,h=32,gap="$space.1",items="center",rounded="$radius.sm",
                                bg=function() return if selected:get()[row.key] then "$zeron.secondary.bg" else "#00000000" end,
                                hover={bg="$hover"},
                                ui.button {label=`{row.group} {row.file.path}`,grow=1,min_w=0,h_full=true,items="center",justify="start",
                                    bg="#00000000",on_key=list_key,on_click=function(e: any) select(row,e) end,
                                    on_mount=function(el: any) row_els[row.key]=el end,
                                    FadeText {if row.file.oldPath then `{row.file.oldPath} → {row.file.path}` else row.file.path,grow=1,color="$text.primary"},
                                },
                                fm.fade(function() return w.staging_pending(row.file.path)~=nil end,
                                    ui.text {function() return if w.staging_pending(row.file.path) then "Staging…" else "Unstaging…" end,text_size="$font.size.xs",color="$text.secondary"}),
                                IconButton {icon=if row.group=="staged" then "minus" else "plus",size="sm",
                                    label=function() local n=#Selection.paths(w.repo(),selected:get(),row.group,false,row)
                                        return `{if row.group=="staged" then "Unstage" else "Stage"} {if n>1 then `selected changes ({n} files)` else row.file.path}` end,
                                    disabled=function() return not w.can_stage(Selection.paths(w.repo(),selected:get(),row.group,false,row)) end,
                                    on_click=function() stage_paths(Selection.paths(w.repo(),selected:peek(),row.group,false,row),row.group~="staged") end},
                                ui.text {letter,w=16,align="center",color=color,font="$font.mono",text_size="$font.size.sm"},
                            },
                        }
                end
                local frame: {[any]: any}={id="git-source-control",ref=list_box,focusable=true,on_key=list_key,w=if p.wide then 300 else "100%",h=if p.wide then "100%" else 180,shrink=0}
                local list: Node
                if kind=="many" then
                    frame.offset=list_offset
                    frame.heights=function() local heights={};for _ in row_list:get() do table.insert(heights,32) end;return heights end
                    frame.key=function(i: number) local r=row_list:get()[i];return if r then `{i}:{r.key}:{r.title or ""}:{w.revision:get()}` else tostring(i) end
                    frame.row=function(key: string): Node
                        local row=row_list:peek()[tonumber(string.match(key,"^(%d+):")) or 0]
                        return if row then file_row(row) else ui.div {}
                    end
                    list=ui.list(frame :: any)
                else
                    -- Each row by its `id`: a file staged or unstaged slides
                    -- to its new group, the rest closing up and making room;
                    -- new files fade in and discarded ones out.
                    table.insert(frame,fm.each(row_list,function(row: any): string return row.id end,file_row,{w_full=true}))
                    list=ui.scroll(frame :: any)
                end
                return (if p.wide then ui.row else ui.column) {w_full=true,grow=1,min_h=0,gap="$space.2",
                list,
                ui.column {grow=1,min_w=0,min_h=0,gap="$space.1",
                    ui.text {function() local row=chosen:get();return if row then row.file.path else "" end,color="$text.secondary",font="$font.mono",text_size="$font.size.sm"},
                    Changes.list {id="git-fork-diff",bare=true,columns=2,offset=diff_offset,
                        files=function() local row=chosen:get();return if row then {Changes.file(row.file.path,"modified",0,0)} else {} end,
                        ranges=true,on_comment=function(path: string,key: string,line: any,range: any?)
                            local rows=range or {line};local old=true;local keys,snippet={},{}
                            for _,item in rows do if item.kind~="-" then old=false end;table.insert(keys,git.line_key(item));table.insert(snippet,item.text) end
                            local first,last=nil,nil
                            for _,item in rows do local number=if old then item.old else item.new;if number then first=first or number;last=number end end
                            comment={file=(if w.repository:peek()~="" then w.repository:peek() .. "/" else "") .. path,
                                line=first or line.old or line.new or 1,to=last,side=if old then "L" else "R",snippet=table.concat(snippet,"\n"),key=ui.json.encode({p=path,k=keys}),session=m.id}
                            note:set("");commenting:set(true)
                        end,
                        marks=function(path: string,key: string): string?
                            for encoded,mark in zeron.composed() do
                                local ok,value=pcall(ui.json.decode,encoded)
                                if ok and value.p==path and (not mark.session or mark.session==m.id) then
                                    for _,line in value.k or {} do if line==key then return if mark.hovered then "lit" else "on" end end
                                end
                            end;return nil
                        end,
                        lines=function(_f: any) diff_version:get();return diff_cache or {} end},
                    fm.show(function() local d=diff:get();return d~=nil and d.truncated end,ui.text {"Partial diff: the file exceeds the display limit.",color="$text.secondary"}),
                },
            }
            end,fm.swap {w_full=true,grow=1,min_h=0}),
            Dialog {open=discard,title="Discard these changes?",tone="danger",body=function() return `Discard {if preview then preview.fileCount else 0} files? A recovery copy will be kept for Undo discard.` end,
                confirm={label="Discard",on_click=function() if preview then w.confirm_discard(preview,discard_pin) end end}},
            Dialog {open=w.conflict_open,title="Stage files with conflict markers?",tone="danger",
                body=function() return `Conflict markers remain in {w.conflict_count or 0} selected files. Resolve them first, or stage the current contents anyway.` end,
                confirm={label="Stage anyway",on_click=w.confirm_stage}},
            Dialog {open=commenting,title="Comment on selected code",size="md",confirm={label="Add to chat",on_click=function()
                if comment and note:peek()~="" then comment.note=note:peek();zeron.compose_comment(comment) end
            end},TextArea {value=note,label="Review comment",rows=3,max_rows=8}},
        }
    end)
end
