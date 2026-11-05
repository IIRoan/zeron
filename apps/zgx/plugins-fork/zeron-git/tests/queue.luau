local calls={}
local saved={}
local zeron={store=function(key,default)
        saved[key]=saved[key] or default
        return {peek=function() return saved[key] end,set=function(_,value) saved[key]=value end}
    end,context_of=function() return {cwd="/test",device="local",project="p"} end,
    workbench=function(method,context,params)
        local call={method=method,params=params};table.insert(calls,call)
        return {after=function(_,ok,bad) call.ok=ok;call.bad=bad end}
    end}
local m={id="queue-test",message=ui.state("test.message",""),status=ui.state("test.status",nil),acting=ui.state("test.acting",false),refresh=function() end}
local w=require("./workbench")(zeron,m,function(e) return {words=tostring(e)} end)
local function baseline(a,b)
    return {{path="",complete=true,files={
        {path="a",index=if a then "modified" else "unchanged",worktree=if a then "unchanged" else "modified"},
        {path="b",index=if b then "modified" else "unchanged",worktree=if b then "unchanged" else "modified"},
    }}}
end
w.baseline=baseline(false,false);w.publish()
w.selected:set({["unstaged:a"]=true,["unstaged:b"]=true});w.anchor:set("unstaged:a")
w.chosen:set({key="unstaged:a",group="unstaged",file=w.repo().files[1]})
-- A poll started before staging may finish late; it cannot undo projection.
w.refresh();local poll=calls[1]
w.stage({"a"},true)
assert(w.repositories:peek()[1].files[1].index=="modified" and w.selected:peek()["staged:a"])
assert(w.chosen:peek().key=="staged:a" and w.anchor:peek()=="staged:a")
assert(w.staging_pending("a")==true and not w.can_stage({"a"}))
w.stage({"a"},false);assert(#w.queue==1,"duplicate pending path is ignored")
w.stage({"b"},true);assert(#w.queue==2 and #calls==2,"independent files queue without parallel index writes")
w.perform("CommitCheckoutStaged",{message="no"});assert(#calls==2,"other mutations wait for index reconciliation")
poll.ok({repositories=baseline(false,false)})
assert(w.repo().files[1].index=="modified","late poll cannot revert staged projection")
calls[2].ok({repositories=baseline(true,false)})
assert(#w.queue==1 and calls[3].params.paths[1]=="b")
assert(w.repo().files[2].index=="modified" and w.staging_pending("b")==true)
calls[3].bad("index locked")
assert(w.busy:peek() and not w.verified:peek() and not w.can_stage({"b"}),"verify the real index before enabling mutations")
calls[4].ok({repositories=baseline(true,false)})
assert(#w.queue==0 and not w.busy:peek() and w.staging_pending("b")==nil)
assert(w.repo().files[1].index=="modified" and w.repo().files[2].index=="unchanged")
assert(w.selected:peek()["staged:a"] and w.selected:peek()["unstaged:b"],"failed batch restores valid selections")
assert(w.error:peek()=="index locked")
w.baseline={{path="",complete=true,files={{path="merge",index="unmerged",worktree="unmerged"}}}};w.publish()
w.stage({"merge"},true)
local check=calls[#calls];assert(check.method=="GetCheckoutConflictMarkers")
check.ok({"merge"})
assert(w.conflict_open:peek() and not w.can_stage({"merge"}),"remaining markers need confirmation")
w.confirm_stage()
assert(calls[#calls].method=="SetCheckoutStaged" and calls[#calls].params.allowConflictMarkers==true)
assert(w.repo().files[1].index=="unmerged","conflict rows wait for Git instead of optimistic projection")

-- A prompt captures the message, working paths and HEAD. Later typing is
-- retained, and a follow-up push must use the commit's new HEAD.
local m2={id="commit-test",message=ui.state("commit.message","Captured message"),status=ui.state("commit.status",{oid="before",branch="main"}),
    acting=ui.state("commit.acting",false),refresh=function() end}
local c=require("./workbench")(zeron,m2,function(e) return {words=tostring(e)} end)
c.baseline={{path="",complete=true,files={{path="draft.txt",index="unchanged",worktree="modified"}}}};c.publish()
assert(c.commit_reason()==nil,"a ready repository with a message and changes can commit")
m2.message:set(" \n ");assert(c.commit_reason()=="Enter a commit message.")
m2.message:set("Captured message")
c.verified:set(false);assert(c.commit_reason()=="Refresh the repository before committing.")
c.verified:set(true);c.busy:set(true)
assert(c.commit_reason()=="Wait for the current Git operation to finish.")
c.busy:set(false)
local draft=c.baseline
c.baseline={{path="",complete=true,files={}}};c.publish()
assert(c.commit_reason()=="There are no changes to commit.")
assert(c.commit_reason(true)==nil,"amend remains available with a clean worktree")
c.baseline={{path="",complete=true,files={{path="merge",index="unmerged",worktree="unmerged"}}}};c.publish()
assert(c.commit_reason()=="Resolve and stage merge conflicts before committing.")
c.baseline=draft;c.publish()
local count=#calls
c.commit(false,"push")
assert(c.commit_ask:peek() and #calls==count,"an empty index never stages before confirmation")
m2.message:set("New message typed while confirming")
c.confirm_commit()
local committing=calls[#calls]
assert(committing.method=="CommitCheckoutStaged" and committing.params.message=="Captured message")
assert(committing.params.stagePaths[1]=="draft.txt" and committing.params.expectedHead=="before")
committing.ok({committed=true,git={head="after",branch="main"}})
assert(m2.message:peek()=="New message typed while confirming","a completed commit cannot erase newer typing")
ui.after(0,function()
    local push=nil
    for _,call in calls do if call.method=="RunCheckoutGitAction" and call.params.action.kind=="push" then push=call end end
    assert(push and push.params.expectedHead=="after" and push.params.expectedBranch=="main","push uses the new commit's pin")
end)
