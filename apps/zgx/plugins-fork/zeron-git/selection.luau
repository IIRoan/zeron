--!strict
-- Index and working-tree entries are separate resources, including when
-- they describe the same path. Match the GPUI source-control selection.
local M = {}

function M.conflict(file: any): boolean
    return file.index == "unmerged" or file.worktree == "unmerged"
        or (file.index == "added" and file.worktree == "added")
        or (file.index == "deleted" and file.worktree == "deleted")
end

function M.groups(file: any): {string}
    if M.conflict(file) then return {"conflicts"} end
    local groups = {}
    if file.index ~= "unchanged" and file.index ~= "untracked" then table.insert(groups, "staged") end
    if file.worktree ~= "unchanged" or file.index == "untracked" then table.insert(groups, "unstaged") end
    return groups
end

function M.rows(repository: any): {[string]: any}
    local out = {}
    for _,file in (repository and repository.files) or {} do
        for _,group in M.groups(file) do
            local key = `{group}:{file.path}`
            out[key] = {key=key, group=group, file=file}
        end
    end
    return out
end

function M.choose(selected: any, anchor: string?, rows: {any}, row: any, event: any): (any, string?)
    local toggle = event.ctrl or event.meta
    local next = if toggle then table.clone(selected) else {}
    if event.shift then
        local first,last = nil,nil
        for i,item in rows do
            if item.key == anchor then first = i end
            if item.key == row.key then last = i end
        end
        if first and last then
            for i=math.min(first,last),math.max(first,last) do
                if rows[i].file then next[rows[i].key] = true end
            end
        else
            next = table.clone(selected); next[row.key] = true; anchor = row.key
        end
    else
        next[row.key] = if toggle and selected[row.key] then nil else true
        anchor = row.key
    end
    return next,anchor
end

function M.paths(repository: any, selected: any, group: string?, all: boolean?, clicked: any?): {string}
    if clicked and not selected[clicked.key] then return {clicked.file.path} end
    local out,seen = {},{}
    for _,file in (repository and repository.files) or {} do
        for _,side in M.groups(file) do
            if (not group or side == group) and (all or selected[`{side}:{file.path}`]) and not seen[file.path] then
                seen[file.path] = true; table.insert(out,file.path)
            end
        end
    end
    table.sort(out)
    return out
end

-- Called for both optimistic moves and Git's authoritative reply. Keep
-- focus and the range anchor on the destination, and prune vanished files.
function M.reconcile(repository: any, selected: any, chosen: any?, anchor: string?, move: any?): (any, any?, string?)
    local rows = M.rows(repository)
    local function destination(key: string): string?
        local group,path = string.match(key,"^([^:]+):(.*)$")
        if move and move.paths[path] then
            local target = `{if move.staged then "staged" else "unstaged"}:{path}`
            if rows[target] then return target end
        end
        if rows[key] then return key end
        local other = `{if group == "staged" then "unstaged" else "staged"}:{path}`
        return if rows[other] then other else nil
    end
    local next = {}
    for key,on in selected do
        local target = if on then destination(key) else nil
        if target then next[target] = true end
    end
    local focus = if chosen then destination(chosen.key) else nil
    return next,if focus then rows[focus] else nil,if anchor then destination(anchor) else nil
end

return M
