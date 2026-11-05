--!strict
-- Porcelain projection from the original GPUI staging queue. Renames,
-- conflicts and gitlinks stay put until Git can tell us their real state.
local M = {}
local function copy(value: any): any
    if type(value) ~= "table" then return value end
    local out = {}; for k,v in value do out[k] = copy(v) end; return out
end

function M.project(baseline: {any}, requests: {any}): {any}
    local repositories = copy(baseline)
    for _,request in requests do
        for _,repository in repositories do
            if repository.path ~= request.pin.params.repository or not repository.complete then continue end
            local paths,submodules = {},{}
            for _,path in request.paths do paths[path] = true end
            for _,path in repository.submodules or {} do submodules[path] = true end
            for _,file in repository.files do
                if not paths[file.path] or submodules[file.path] or file.oldPath
                    or file.index == "unmerged" or file.worktree == "unmerged"
                    or (file.index == "added" and file.worktree == "added")
                    or (file.index == "deleted" and file.worktree == "deleted")
                    or file.index == "renamed" or file.index == "copied"
                    or file.worktree == "renamed" or file.worktree == "copied" then continue end
                if request.staged then
                    if file.worktree == "unchanged" then continue end
                    file.index = if file.worktree == "untracked" or file.worktree == "added" then "added"
                        elseif file.worktree == "modified" and file.index == "added" then "added"
                        elseif file.worktree == "deleted" and file.index == "added" then "unchanged" else file.worktree
                    file.worktree = "unchanged"
                else
                    if file.index == "unchanged" or file.index == "untracked" then continue end
                    file.worktree = if file.index == "added" and file.worktree == "deleted" then "unchanged"
                        elseif file.index == "added" then "untracked"
                        elseif file.worktree == "unchanged" then file.index else file.worktree
                    file.index = "unchanged"
                end
            end
            local files = {}
            for _,file in repository.files do
                if file.index ~= "unchanged" or file.worktree ~= "unchanged" then table.insert(files,file) end
            end
            repository.files = files
        end
    end
    return repositories
end

return M
