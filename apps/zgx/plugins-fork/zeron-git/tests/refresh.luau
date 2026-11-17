local calls={}
local saved={}
local zeron={
    store=function(key,default)
        if saved[key]==nil then saved[key]=default end
        return {peek=function() return saved[key] end,set=function(_,value) saved[key]=value end}
    end,
    context_of=function() return {cwd="/checkout",device="local",project="p"} end,
    workbench=function(method,context,params)
        local c={method=method,context=context,params=params};table.insert(calls,c)
        return {after=function(_,ok,bad) c.ok=ok;c.bad=bad end}
    end,
}
local m={id="refresh-test",status=ui.state("test.status",nil),acting=ui.state("test.acting",nil)}
local w=require("./workbench")(zeron,m,function(e) return {words=tostring(e)} end)
m.refresh=function(force) assert(force);w.refresh() end
w.repository:set("nested")
w.baseline={{path="nested",complete=true,files={},git={head="abc",branch="main",upstream="origin/main",ahead=2,behind=3}}}
w.publish()
w.refresh_remote()
assert(#calls==1 and calls[1].method=="RunCheckoutGitAction")
assert(calls[1].params.action.kind=="fetch" and calls[1].params.repository=="nested")
assert(calls[1].context.scope.cwd=="/checkout" and calls[1].params.expectedHead=="abc")
assert(w.busy:peek(),"refresh stays busy until fetch completes")
w.refresh_remote();assert(#calls==1,"duplicate refresh cannot start another fetch")
calls[1].ok({})
assert(not w.busy:peek() and calls[2].method=="GetCheckoutChanges" and calls[3].method=="GetCheckoutGitDetails",
    "both status and details must be reread after fetch, never before")
calls[2].ok({repositories={{path="nested",complete=true,files={},git={head="abc",branch="main",upstream="origin/main",ahead=4,behind=5}}}})
calls[3].ok({state={ahead=4,behind=5},incoming={},outgoing={},recent={}})
assert(w.repo().git.ahead==4 and w.repo().git.behind==5,"both counts update together")
assert(w.details:peek().state.ahead==4 and w.details:peek().state.behind==5)
-- A failed local read must not make the explicit refresh unusable.
w.verified:set(false)
w.refresh_remote();assert(calls[4].params.action.kind=="fetch")
calls[4].bad("remote unavailable")
assert(not w.busy:peek() and w.error:peek()=="remote unavailable")
assert(calls[5].method=="GetCheckoutChanges" and calls[6].method=="GetCheckoutGitDetails")
assert(w.repo().git.ahead==4 and w.repo().git.behind==5,"failure preserves the last known counts")
-- Index jobs and another operation still block overlapping refreshes.
w.busy:set(true);w.refresh_remote();assert(#calls==6)
w.busy:set(false);m.acting:set("pull");w.refresh_remote();assert(#calls==6)

-- A failed poll started before fetch must not swallow the requested reread.
local n={id="refresh-race",status=ui.state("race.status",nil),acting=ui.state("race.acting",nil)}
local race=require("./workbench")(zeron,n,function(e) return {words=tostring(e)} end)
n.refresh=function() race.refresh() end
local start=#calls
race.refresh();local poll=calls[start+1]
race.refresh_remote();local fetch=calls[start+2]
fetch.ok({})
assert(calls[start+3].method=="GetCheckoutGitDetails")
poll.bad("stale poll failed")
assert(calls[start+4].method=="GetCheckoutChanges","retry the post-fetch status read after a late poll failure")
assert(race.error:peek()=="","an obsolete poll cannot replace the refresh result")
