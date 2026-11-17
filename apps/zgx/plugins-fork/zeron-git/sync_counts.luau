--!strict
-- VS Code's compact sync affordance: a single action with number-first
-- incoming/outgoing counts, or Publish Branch before tracking is set up.
local Button = require("@ds/button")
local Icon = require("@ds/icon")
local Tooltip = require("@ds/tooltip")

return function(m: any): Node
    local w = m.workbench
    local function status(): any
        local r = w.repo()
        return r and r.complete and r.git
    end
    local function busy(): boolean
        return w.busy:get() or m.acting:get() ~= nil
    end
    local function mode(): string
        local s = status()
        if not s then return "loading" end
        if not s.branch then return "detached" end
        if not s.head then return "empty" end
        return if s.upstream then "sync" else "publish"
    end
    local function remote(): string?
        local d = w.details:get()
        local s = status()
        if not d or not s or not d.state or d.state.head ~= s.head or d.state.branch ~= s.branch then return nil end
        return d.publishRemote
    end
    local function counts_known(): boolean
        local s = status()
        return s ~= nil and type(s.ahead) == "number" and type(s.behind) == "number"
    end
    local function label(): string
        local s = status()
        local kind = mode()
        if kind == "loading" then return "Reading sync status" end
        if kind == "publish" then return "Publish Branch" end
        if kind == "empty" then return "Create a commit before publishing this branch" end
        if kind == "detached" then return "Check out a branch to sync changes" end
        if not counts_known() then return "Fetch to read incoming and outgoing commits" end
        return `Sync Changes ({s.behind} incoming, {s.ahead} outgoing)`
    end
    local function disabled(): boolean
        if busy() or not w.verified:get() or w.conflict_open:get() then return true end
        local kind = mode()
        if kind == "publish" then return remote() == nil end
        if kind ~= "sync" or not counts_known() then return true end
        local s = status()
        return s.ahead == 0 and s.behind == 0
    end
    local function counter(id: string, field: string, icon: string): Node
        return ui.row {
            id = id, shrink = 0, items = "center", gap = "$zeron.space.hair",
            ui.text { function(): string
                local s = status()
                return if s and type(s[field]) == "number" then tostring(s[field]) else ""
            end,
                text_size = "$font.size.xs", tabular = true },
            Icon { name = icon :: any, size = 12 },
        }
    end
    return ui.presence(mode, function(kind: string): Node
        return Button {
            id = "git-sync", label = label,
            tooltip = Tooltip { function(): string
                if busy() then return "Git operation in progress" end
                if kind == "publish" then
                    if remote() then return `Publish branch to {remote()}` end
                    local d = w.details:get()
                    return if d and #(d.remotes or {}) > 0 then "Choose a publish remote from More" else "Add a remote to publish this branch"
                end
                local s = status()
                if kind == "sync" and counts_known() then
                    if s.ahead == 0 and s.behind == 0 then return `Up to date with {s.upstream}` end
                    return `{label()} · Pull, then push to {s.upstream}`
                end
                return label()
            end },
            size = "xs", tone = "ghost", rounded = "$radius.sm",
            px = "$space.1", gap = "$space.1", shrink = 0,
            disabled = disabled, busy = busy,
            on_click = function()
                if disabled() or kind ~= mode() then return end
                if kind == "publish" then w.action({ kind = "publish", remote = remote() })
                else w.action({ kind = "sync" }) end
            end,
            Icon { name = if kind == "publish" then "upload-minimalistic" else "refresh", size = 14 },
            if kind == "publish" then ui.text { "Publish Branch", text_size = "$font.size.xs" }
            elseif kind == "sync" then ui.presence(counts_known, function(known: boolean): Node
                return if known then ui.row {
                    id = "git-sync-counts", items = "center", gap = "$space.1",
                    counter("git-incoming-count", "behind", "arrow-down"),
                    counter("git-outgoing-count", "ahead", "arrow-up"),
                } else ui.text { "Sync Changes", text_size = "$font.size.xs" }
            end)
            else ui.text { "Sync Changes", text_size = "$font.size.xs" },
        }
    end, { shrink = 0 })
end
