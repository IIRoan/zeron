local S=require("./staging")
local function file(path,index,worktree,extra)
    local f={path=path,index=index,worktree=worktree};for k,v in extra or {} do f[k]=v end;return f
end
local baseline={{path="",complete=true,submodules={"sub"},files={
    file("new","unchanged","untracked"),file("partial","added","modified"),
    file("cancelled","added","deleted"),file("removed","unchanged","deleted"),
    file("sub","modified","modified"),file("conflict","unmerged","unmerged"),
    file("renamed","renamed","modified",{oldPath="old"}),
}}}
local paths={};for _,f in baseline[1].files do table.insert(paths,f.path) end
local on={paths=paths,staged=true,pin={params={repository=""}}}
local projected=S.project(baseline,{on})
local by={};for _,f in projected[1].files do by[f.path]=f end
assert(by.new.index=="added" and by.new.worktree=="unchanged")
assert(by.partial.index=="added" and by.partial.worktree=="unchanged")
assert(by.cancelled==nil and by.removed.index=="deleted")
assert(by.sub.worktree=="modified" and by.conflict.index=="unmerged" and by.renamed.worktree=="modified")
assert(baseline[1].files[1].worktree=="untracked","projection must leave baseline untouched")
local off={paths={"new","partial","removed"},staged=false,pin={params={repository=""}}}
projected=S.project(projected,{off});by={};for _,f in projected[1].files do by[f.path]=f end
assert(by.new.index=="unchanged" and by.new.worktree=="untracked")
assert(by.partial.worktree=="untracked" and by.removed.worktree=="deleted")
assert(S.project(baseline,{{paths={"new"},staged=true,pin={params={repository="other"}}}})[1].files[1].index=="unchanged")
