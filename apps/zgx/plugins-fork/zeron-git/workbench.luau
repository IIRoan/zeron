--!strict
-- Source control shares the fork engine's Git implementation. Captured
-- checkout/HEAD/branch values travel with every queued or prompted action.
local Selection = require("./selection")
local Staging = require("./staging")
return function(zeron: any,m: any,problem: any)
    local w: any={repositories=ui.state(`git.{m.id}.repositories`,{}),repository=ui.state(`git.{m.id}.repository`,""),
        details=ui.state(`git.{m.id}.details`,nil :: any),error=ui.state(`git.{m.id}.native_error`,""),
        revision=ui.state(`git.{m.id}.native_revision`,0),busy=ui.state(`git.{m.id}.native_busy`,false),
        amend=ui.state(`git.{m.id}.amend`,false),
        selected=ui.state(`git.{m.id}.selected`,{} :: any),chosen=ui.state(`git.{m.id}.chosen`,nil :: any),
        anchor=ui.state(`git.{m.id}.anchor`,nil :: any),pending=ui.state(`git.{m.id}.staging_pending`,{} :: any),
        verified=ui.state(`git.{m.id}.index_verified`,true),conflict_open=ui.state(`git.{m.id}.stage_conflicts`,false),
        recovery=ui.persist(`git.{m.id}.discard_recovery`,nil :: any),
        command_request=ui.state(`git.{m.id}.command_request`,nil :: any),commit_ask=ui.state(`git.{m.id}.commit_ask`,false),
        queue={},epoch=0,loading=false,baseline={}}
    function w.context(): any
        local s=zeron.context_of("session",m.id)
        return {session=m.id,scope={cwd=s and s.cwd,device=s and s.device,project=s and s.project}}
    end
    function w.repo(): any?
        for _,r in w.repositories:get() do if r.path==w.repository:get() then return r end end
        return nil
    end
    function w.capture(): any
        local r=w.repo();local s=if r then r.git else nil
        local old=m.status:peek()
        return {context=w.context(),params={repository=w.repository:peek(),
            expectedHead=(s and s.head) or (old and old.oid) or ui.null,
            expectedBranch=(s and s.branch) or (old and old.branch) or ui.null}}
    end
    function w.call(method: string,params: any?,pin: any?): any
        local captured=pin or w.capture()
        local p=table.clone(captured.params);for k,v in params or {} do p[k]=v end
        return zeron.workbench(method,captured.context,p)
    end
    local drafts=zeron.store("commit_drafts",{})
    local last_draft: string?=nil
    local function draft_key(pin: any?): string
        local c=if pin then pin.context else w.context()
        local repo=if pin then pin.params.repository else w.repository:get()
        return `{c.scope.device}:{c.scope.cwd}:{repo}`
    end
    local function read_draft(key: string): string?
        local value=zeron.store(`commit_draft:{key}`,nil):peek()
        return if value~=nil then value else drafts:peek()[key]
    end
    local function keep_draft(key: string,text: string)
        -- Separate store keys avoid two visible repositories replacing each
        -- other's map while the host publishes their writes.
        if read_draft(key)~=text then zeron.store(`commit_draft:{key}`,nil):set(text) end
    end
    function w.watch_draft()
        -- A model outlives its tab. Effects must belong to each mounted view,
        -- so switching to Files and back resumes saving the cached model.
        ui.effect(function()
            local key=draft_key()
            if key~=last_draft then
                if last_draft then keep_draft(last_draft,m.message:peek()) end
                local kept=read_draft(key)
                if kept~=nil then m.message:set(kept) elseif last_draft then m.message:set("") end
                last_draft=key
            end
            keep_draft(key,m.message:get())
        end)
        ui.on_cleanup(function() if last_draft then keep_draft(last_draft,m.message:peek()) end end)
    end
    function w.publish(move: any?)
        local repositories=Staging.project(w.baseline,w.queue)
        w.repositories:set(repositories)
        local repository=w.repo()
        local selected,chosen,anchor=Selection.reconcile(repository,w.selected:peek(),w.chosen:peek(),w.anchor:peek(),move)
        w.selected:set(selected);w.chosen:set(chosen);w.anchor:set(anchor)
        local pending={}
        for _,job in w.queue do
            for _,path in job.paths do pending[`{job.pin.params.repository}:{path}`]=job.staged end
        end
        w.pending:set(pending);w.revision:add(1)
    end
    function w.staging_pending(path: string): boolean?
        return w.pending:get()[`{w.repository:get()}:{path}`]
    end
    function w.can_stage(paths: {string}): boolean
        if not w.verified:get() or w.conflict_open:get() or m.acting:get() or (w.busy:get() and #w.queue==0) then return false end
        local r=w.repo();if not r or not r.complete then return false end
        if w.busy:get() then
            for _,file in r.files do
                if Selection.conflict(file) and table.find(paths,file.path) then return false end
            end
        end
        for _,path in paths do if w.staging_pending(path)==nil then return true end end
        return false
    end
    function w.refresh()
        if #w.queue>0 then w.again=true;return end
        if w.loading then w.again=true;return end
        w.loading=true
        local context=w.context();local repository=w.repository:peek();local epoch=w.epoch
        zeron.workbench("GetCheckoutChanges",context,{}):after(function(result: any)
            w.loading=false
            if epoch==w.epoch then
                w.verified:set(true)
                local status=m.status:peek() or {}
                local signature=ui.json.encode(result.repositories or {}) .. (status.tree or "") .. (status.index or "")
                local changed=signature~=w.signature
                w.signature=signature
                if changed then w.baseline=result.repositories or {};w.publish() end
                local found=false;for _,r in result.repositories or {} do if r.path==repository then found=true end end
                if not found then w.repository:set("") end
            end
            if w.again or epoch~=w.epoch then w.again=false;w.refresh() end
        end,function(e: any) w.loading=false;if epoch==w.epoch then w.verified:set(false);w.error:set(problem(e).words) end end)
    end
    function w.load_details()
        local pin=w.capture()
        w.call("GetCheckoutGitDetails",{},pin):after(function(d: any)
            if pin.params.repository==w.repository:peek() then w.details:set(d) end
        end,function(e: any) w.error:set(problem(e).words) end)
    end
    function w.perform(method: string,params: any?,done: ((any)->())?,pin: any?)
        if w.busy:peek() or #w.queue>0 or w.conflict_open:peek() or not w.verified:peek() or m.acting:peek() then return end
        local r=w.repo();if not r or not r.complete then w.error:set("Refresh the repository before changing it.");return end
        local captured=pin or w.capture()
        w.busy:set(true);w.epoch+=1;w.error:set("")
        w.call(method,params,captured):after(function(value: any)
            w.epoch+=1;w.busy:set(false)
            if done then done(value) end
            w.refresh();m.refresh(true);w.load_details()
        end,function(e: any)
            w.epoch+=1;w.busy:set(false);w.error:set(problem(e).words);w.refresh()
        end)
    end
    function w.action(action: any,pin: any?,done: ((any)->())?)
        w.perform("RunCheckoutGitAction",{action=action},done,pin)
    end
    local function commit(job: any,stage_paths: {string})
        w.perform("CommitCheckoutStaged",{message=job.message,amend=job.amend,stagePaths=stage_paths},function(result: any)
            local key=draft_key(job.pin)
            if read_draft(key)==job.message then keep_draft(key,"") end
            if last_draft==key and m.message:peek()==job.message then m.message:set("") end
            w.amend:set(false)
            if job.follow then
                if not result.git or not result.git.head then w.error:set("Committed. Refresh before pushing or syncing.");return end
                local next={context=job.pin.context,params=table.clone(job.pin.params)}
                next.params.expectedHead=result.git.head;next.params.expectedBranch=result.git.branch or ui.null
                ui.after(0,function() w.action({kind=job.follow},next) end)
            end
        end,job.pin)
    end
    function w.commit_reason(amend: boolean?): string?
        if w.busy:get() or m.acting:get() or #w.queue>0 or w.conflict_open:get() then return "Wait for the current Git operation to finish." end
        local r=w.repo()
        if not w.verified:get() or not r or not r.complete then return "Refresh the repository before committing." end
        if not string.find(m.message:get(),"%S") then return "Enter a commit message." end
        for _,file in r.files do if Selection.conflict(file) then return "Resolve and stage merge conflicts before committing." end end
        if not (if amend==nil then w.amend:get() else amend) and #Selection.paths(r,{},"staged",true)==0 and #Selection.paths(r,{},"unstaged",true)==0 then return "There are no changes to commit." end
        return nil
    end
    function w.commit(amend: boolean,follow: string?,pin: any?)
        local why=w.commit_reason(amend)
        if why then w.error:set(why);return end
        local r=w.repo();if not r or not r.complete then return end
        local job={message=m.message:peek(),amend=amend,follow=follow,pin=pin or w.capture()}
        local staged=Selection.paths(r,{},"staged",true)
        local working=Selection.paths(r,{},"unstaged",true)
        if not amend and #staged==0 and #working>0 then
            job.paths=working;w.commit_job=job;w.commit_ask:set(true)
        else commit(job,{}) end
    end
    function w.confirm_commit()
        local job=w.commit_job;w.commit_job=nil;w.commit_ask:set(false)
        if job then commit(job,job.paths) end
    end
    function w.toggle_amend(wanted: boolean?)
        if w.busy:peek() then return end
        local on=if wanted==nil then not w.amend:peek() else wanted
        w.amend:set(on)
        if on and m.message:peek()=="" then
            w.call("GetCheckoutGitDetails",{}):after(function(d: any)
                if w.amend:peek() and m.message:peek()=="" then m.message:set(d.lastMessage or "") end
            end,function(e: any) w.error:set(problem(e).words) end)
        end
    end
    local function drain()
        if w.busy:peek() or #w.queue==0 then return end
        local job=w.queue[1]
        w.busy:set(true);w.epoch+=1;w.error:set("")
        w.call("SetCheckoutStaged",{paths=job.paths,staged=job.staged,allowConflictMarkers=job.allow_markers or false},job.pin):after(function(result: any)
            table.remove(w.queue,1)
            w.baseline=result.repositories or w.baseline
            w.signature=nil;w.epoch+=1;w.busy:set(false);w.publish()
            if job.done then job.done(result) end
            if #w.queue>0 then drain() else w.refresh();m.refresh(true) end
        end,function(e: any)
            table.remove(w.queue,1)
            w.epoch+=1;w.verified:set(false);w.error:set(problem(e).words);w.publish()
            if job.failed then job.failed(e) end
            -- Git may have written before its reply was lost. Re-read the
            -- real index before another queued write or a commit is allowed.
            zeron.workbench("GetCheckoutChanges",job.pin.context,{}):after(function(result: any)
                w.baseline=result.repositories or {};w.signature=nil;w.epoch+=1;w.verified:set(true)
                w.busy:set(false);w.publish()
                if #w.queue>0 then drain() else w.refresh();m.refresh(true) end
            end,function(read_error: any)
                w.queue={};w.busy:set(false);w.publish()
                w.error:set(`Unable to verify the index: {problem(read_error).words}. Refresh before changing files.`)
            end)
        end)
    end
    local function enqueue(job: any)
        local paths={};for _,path in job.paths do paths[path]=true end
        table.insert(w.queue,job)
        w.epoch+=1;w.publish({paths=paths,staged=job.staged});drain()
    end
    function w.stage(paths: {string},staged: boolean,done: any?,failed: any?): boolean?
        if not w.can_stage(paths) then return end
        local r=w.repo();if not r or not r.complete then w.error:set("Refresh the repository before staging files.");return end
        local accepted,seen={},{}
        for _,path in paths do
            if not seen[path] and w.staging_pending(path)==nil then seen[path]=true;table.insert(accepted,path) end
        end
        if #accepted==0 then return end
        local job={paths=accepted,staged=staged,pin=w.capture(),done=done,failed=failed}
        local conflicts=false
        for _,file in r.files do if staged and seen[file.path] and Selection.conflict(file) then conflicts=true end end
        if conflicts then
            w.busy:set(true);w.epoch+=1
            w.call("GetCheckoutConflictMarkers",{paths=accepted},job.pin):after(function(markers: any)
                w.busy:set(false);w.epoch+=1
                if #markers==0 then enqueue(job)
                else w.conflict_job=job;w.conflict_count=#markers;w.conflict_open:set(true) end
            end,function(e: any) w.busy:set(false);w.error:set(problem(e).words);w.refresh() end)
        else enqueue(job) end
        return true
    end
    function w.confirm_stage()
        local job=w.conflict_job;w.conflict_job=nil;w.conflict_open:set(false)
        if job then job.allow_markers=true;enqueue(job) end
    end
    function w.discard(paths: {string},include_staged: boolean,pin: any,ready: (any)->())
        w.call("PreviewCheckoutDiscard",{paths=paths,includeStaged=include_staged},pin):after(ready,function(e: any) w.error:set(problem(e).words) end)
    end
    function w.confirm_discard(preview: any,pin: any)
        w.perform("DiscardCheckoutChanges",{preview=preview},function(result: any)
            w.recovery:set({context=pin.context,params=pin.params,result=result})
        end,pin)
    end
    function w.undo()
        local recovery=w.recovery:peek();if not recovery then return end
        w.perform("RestoreCheckoutDiscard",{recoveryId=recovery.result.recoveryId,checksum=recovery.result.checksum},function() w.recovery:set(nil) end,recovery)
    end
    -- Keep upstream History, branch/turn diffs and plugin slots. Their
    -- existing controls use the same guarded mutations as this view.
    function w.intercept(method: string,args: any?): any?
        local a=args or {};local pin=w.capture();local native=nil;local params={}
        if method=="stage" or method=="unstage" then
            local staged=method=="stage"
            local paths=if a.all then Selection.paths(w.repo(),{},if staged then "unstaged" else "staged",true) else a.paths or {}
            return {after=function(_self: any,ok: any,bad: any)
                if not w.stage(paths,staged,ok,bad) and bad then bad("Wait for the index to finish updating, then try again.") end
            end}
        elseif method=="commit" then native="CommitCheckoutStaged";params={message=a.message or "",amend=a.amend or false,stagePaths={}}
        elseif method=="push" or method=="pull" or method=="fetch" or method=="switch" then
            native="RunCheckoutGitAction"
            local action: any={kind=method}
            if method=="switch" then action=if a.create then {kind="createBranch",name=a.branch} else {kind="switchBranch",branch=a.branch}
            elseif method=="push" and a.publish then
                local d=w.details:peek();action={kind="publish",remote=(d and d.publishRemote) or a.remote or "origin"}
            end
            params={action=action}
        elseif method=="discard" then
            if w.busy:peek() or not w.verified:peek() or w.conflict_open:peek() then
                return {after=function(_self: any,_ok: any,bad: any) if bad then bad("Wait for the index to finish updating.") end end}
            end
            -- The upstream UI has already shown its confirmation. Still
            -- preview immediately before applying and keep the undo record.
            return {after=function(_self: any,ok: any,bad: any)
                local paths=a.paths or {};if a.all then local r=w.repo();for _,f in (r and r.files) or {} do table.insert(paths,f.path) end end
                w.call("PreviewCheckoutDiscard",{paths=paths,includeStaged=true},pin):after(function(preview: any)
                    w.call("DiscardCheckoutChanges",{preview=preview},pin):after(function(result: any)
                        w.recovery:set({context=pin.context,params=pin.params,result=result});if ok then ok(result) end
                    end,bad)
                end,bad)
            end}
        end
        if not native then return nil end
        if w.busy:peek() or not w.verified:peek() or w.conflict_open:peek() then
            return {after=function(_self: any,_ok: any,bad: any) if bad then bad("Wait for the index to finish updating.") end end}
        end
        return w.call(native,params,pin)
    end
    return w
end
