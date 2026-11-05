local S=require("./selection")
local function file(path,index,worktree) return {path=path,index=index or "unchanged",worktree=worktree or "modified"} end
local repository={files={file("a.rs","modified","modified"),file("b.rs"),file("c.rs")}}
local by=S.rows(repository)
local rows={{key="staged"},by["staged:a.rs"],{key="unstaged"},by["unstaged:a.rs"],by["unstaged:b.rs"],by["unstaged:c.rs"]}
local selected,anchor=S.choose({},nil,rows,rows[4],{})
selected,anchor=S.choose(selected,anchor,rows,rows[6],{shift=true})
assert(selected["unstaged:a.rs"] and selected["unstaged:b.rs"] and selected["unstaged:c.rs"])
selected,anchor=S.choose(selected,anchor,rows,rows[5],{ctrl=true})
assert(not selected["unstaged:b.rs"] and selected["unstaged:a.rs"] and selected["unstaged:c.rs"])
selected,anchor=S.choose(selected,anchor,rows,rows[2],{ctrl=true})
assert(#S.paths(repository,selected,"staged")==1 and #S.paths(repository,selected,"unstaged")==2)
assert(#S.paths(repository,selected,"unstaged",false,rows[4])==2,"selected row stages its whole group selection")
assert(#S.paths(repository,selected,"unstaged",false,rows[5])==1,"hovering an unselected row only stages that row")
selected,anchor=S.choose(selected,anchor,rows,rows[6],{shift=true})
assert(selected["staged:a.rs"] and selected["unstaged:b.rs"] and selected["unstaged:c.rs"],"a range can cross index/worktree groups")
selected,anchor=S.choose(selected,anchor,rows,rows[4],{ctrl=true})
selected,anchor=S.choose(selected,anchor,rows,rows[6],{shift=true})
assert(not selected["staged:a.rs"] and selected["unstaged:b.rs"] and selected["unstaged:c.rs"],"shift replaces range")
-- A stable key anchor follows reordered/filtered rows, not its old index.
local reversed={rows[6],rows[5],rows[4]}
selected,anchor=S.choose({},"unstaged:c.rs",reversed,rows[4],{shift=true})
assert(selected["unstaged:a.rs"] and selected["unstaged:b.rs"] and selected["unstaged:c.rs"])
local moved={files={file("a.rs","modified","unchanged"),file("b.rs","modified","unchanged"),file("c.rs")}}
local chosen,next_anchor
selected,chosen,next_anchor=S.reconcile(moved,selected,rows[4],"unstaged:a.rs",{paths={["a.rs"]=true,["b.rs"]=true},staged=true})
assert(selected["staged:a.rs"] and selected["staged:b.rs"] and selected["unstaged:c.rs"])
assert(chosen.key=="staged:a.rs" and next_anchor=="staged:a.rs")
selected,chosen,next_anchor=S.reconcile({files={file("c.rs")}},selected,chosen,next_anchor)
assert(not selected["staged:a.rs"] and selected["unstaged:c.rs"] and chosen==nil and next_anchor==nil)
