-- The GPUI picker's operation filters, full refs, recent list and fuzzy search.
local M={}
function M.ref(b: any): string return `refs/{if b.remote then "remotes" else "heads"}/{b.name}` end
function M.allowed(kind: string,b: any): boolean
    if kind=="pullFrom" then return b.remote==true end
    if kind=="merge" or kind=="rebase" then return not b.current end
    if kind=="deleteBranch" then return not b.remote and not b.current end
    return true
end
function M.score(name: string,query: string): number?
    local score=0;name=string.lower(name)
    for word in string.gmatch(string.lower(query),"%S+") do
        if name==word then continue end
        if string.sub(name,1,#word)==word then score+=1
        elseif string.find(name,word,1,true) then score+=2
        else
            local at=1
            for i=1,#word do local found=string.find(name,string.sub(word,i,i),at,true);if not found then return nil end;at=found+1 end
            score+=3
        end
    end
    return score
end
function M.list(branches: {any},kind: string,query: string,all: boolean,now: number): {any}
    local rows={};local searching=string.find(query,"%S")~=nil
    for index,b in branches do
        local rank=M.score(b.name,query)
        if rank~=nil and M.allowed(kind,b) and (searching or all or b.current or (b.lastUsedAt or 0)>=now-30*86400 or (b.commitTime or 0)>=now-30*86400) then
            table.insert(rows,{branch=b,rank=rank,index=index})
        end
    end
    table.sort(rows,function(a,b) return if a.rank==b.rank then a.index<b.index else a.rank<b.rank end)
    local result={};for _,row in rows do if not searching and not all and #result>=8 then break end;table.insert(result,row.branch) end
    return result
end
return M
