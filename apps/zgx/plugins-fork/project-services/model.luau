--!strict
return function(zeron: any)
    local M = {}
    local models: {[string]: any} = {}
    function M.clone(value: any): any
        return ui.json.decode(ui.json.encode(value))
    end
    function M.context(session: string?, project: string?, checkout: string?): any?
        local chat = if session then zeron.context_of("session", session) else nil
        project = (chat and chat.project) or project
        local p = if project then zeron.context_of("project", project) else nil
        if not p or not p.path or not p.device then return nil end
        local cwd = (chat and chat.cwd) or p.path
        return { session = session, project = project, name = p.name or "Project", root = p.path,checkoutPath=checkout,
            scope = {device = p.device, project = project, cwd = cwd},
            key = `{p.device}:{project}:{checkout or cwd}:{session or "project"}` }
    end
    function M.message(err: any): string
        return if type(err) == "table" and err.message then tostring(err.message) else tostring(err)
    end
    function M.database(service: any): boolean
        return service.setupPrerequisite ~= nil
    end
    function M.get(context: any): any
        if models[context.key] then return models[context.key] end
        local key = context.key
        local location=context.checkoutPath or context.scope.cwd
        local m: any = { context = context, snapshot = ui.state(`services.{key}.snapshot`, nil :: any),
            selected = ui.state(`services.{context.scope.device}:{context.project}:{location}.selected`, ""),
            pending = ui.state(`services.{context.scope.device}:{context.project}:{location}.pending`, {} :: {[string]: boolean}),
            error = ui.state(`services.{key}.error`, ""), editor = ui.state(`services.{key}.editor`, false),
            saving = ui.state(`services.{context.scope.device}:{context.project}:{location}.saving`, false), loading = false, epoch = 0, refresh_again = false }
        models[key] = m
        function m.call(method: string, params: any?): any
            local scoped=table.clone(params or {})
            if context.checkoutPath then scoped.checkoutPath=context.checkoutPath end
            return zeron.workbench(method, context, scoped)
        end
        function m.busy(id: string?): boolean
            local pending = m.pending:get()
            if m.saving:get() or pending["*"] then return true end
            if id then return pending[id] == true end
            return next(pending) ~= nil
        end
        local function accept(snapshot: any)
            m.snapshot:set(snapshot)
            local chosen = m.selected:peek()
            local found = false
            for _, s in snapshot.config.services do if s.id == chosen then found = true end end
            if not found then m.selected:set(if snapshot.config.services[1] then snapshot.config.services[1].id else "") end
        end
        function m.refresh()
            if m.loading then m.refresh_again = true; return end
            m.loading = true
            local epoch = m.epoch
            m.call("GetProjectTerminals"):after(function(snapshot: any)
                m.loading = false
                if epoch == m.epoch then accept(snapshot) end
                if m.refresh_again or epoch ~= m.epoch then m.refresh_again = false; m.refresh() end
            end, function(err: any)
                m.loading = false
                if epoch == m.epoch then m.error:set(M.message(err)) end
            end)
        end
        function m.control(action: string, id: string?)
            if m.busy(id) then return end
            local which = id or "*"
            local pending = table.clone(m.pending:peek()); pending[which] = true; m.pending:set(pending)
            m.epoch += 1
            local epoch = m.epoch
            m.error:set("")
            local function settled()
                -- A poll started before this completion must not overwrite
                -- the state of a service that has just finished starting.
                m.epoch += 1
                local next_pending = table.clone(m.pending:peek()); next_pending[which] = nil; m.pending:set(next_pending)
                m.refresh()
            end
            m.call("ControlProjectTerminals", {action = action, serviceId = id or ui.null}):after(function(snapshot: any)
                if epoch == m.epoch then accept(snapshot) end
                settled()
            end, function(err: any)
                m.error:set(M.message(err)); settled()
            end)
        end
        function m.save(services: any, done: () -> ())
            if m.busy(nil) then return end
            m.saving:set(true); m.epoch += 1; m.error:set("")
            m.call("SaveProjectTerminals", {config = {services = services}}):after(function(snapshot: any)
                m.epoch += 1; accept(snapshot); m.saving:set(false); done()
            end, function(err: any) m.saving:set(false); m.error:set(M.message(err)) end)
        end
        function m.inherit(done: () -> ())
            if m.busy(nil) then return end
            m.saving:set(true); m.epoch += 1
            m.call("SaveProjectTerminals", {useProjectServices = true}):after(function(snapshot: any)
                m.epoch += 1; accept(snapshot); m.saving:set(false); done()
            end, function(err: any) m.saving:set(false); m.error:set(M.message(err)) end)
        end
        return m
    end
    function M.run(snapshot: any, id: string): any?
        if snapshot then for _, run in snapshot.runs do if run.serviceId == id then return run end end end
        return nil
    end
    function M.active(run: any?): boolean
        return run ~= nil and (run.status == "running" or run.status == "restarting")
    end
    function M.label(run: any?): string
        if not run then return "Stopped" end
        if run.readiness == "checking" then return "Checking readiness" end
        if run.readiness == "notReady" then return "Not ready" end
        if run.readiness == "ready" and M.active(run) then return "Ready" end
        local words = {running="Running",restarting="Restarting",stopped="Stopped",exited="Exited",failed="Failed",interrupted="Interrupted"}
        return (words :: any)[run.status] or run.status
    end
    return M
end
