--!strict
-- (Fork) How these views move what they show, as Zeron's own lists and
-- panels do: @ds/motion's pace and ease, and its Reduce motion, under which
-- a move lands at once and a fade is short. Each is read as its part is
-- built. (The same file in each fork plugin: a plugin can't require
-- another's.)
local motion = require("@ds/motion")

local M = {}

local function fade_in(): any
	return motion.transition({ opacity = { 0, 1 }, duration = motion.BASE, ease = motion.EASE })
end

-- `ui.each`'s options with a list's motion (the sidebar's, the toasts'):
-- rows that stay slide to their new places, new ones fade in, and leaving
-- ones fade while the rows after them close up. Rows need a `key`.
function M.rows(opts: { [string]: any }): { [string]: any }
	local o = table.clone(opts)
	o.animate = motion.layout({ duration = motion.BASE, ease = motion.EASE })
	o.enter = fade_in()
	o.exit = motion.transition({ opacity = { 1, 0 }, duration = motion.FADE, ease = motion.EASE })
	return o
end

-- A list (a state of items) with that motion, a row an item by `key(item)`:
-- `build(item)` makes its row, made again as its item changes. (A moving
-- `ui.each` builds a row once, and passes no row state: the row reads its
-- item again by its key. A leaving row keeps what it last showed.)
function M.each(list: any, key: (any) -> string, build: (any) -> Node, opts: { [string]: any }?): Node
	local by_key = ui.computed(function(): { [string]: any }
		local out = {}
		for _, item in list:get() do
			out[key(item)] = item
		end
		return out
	end)
	local o = M.rows(opts or {})
	o.key = key
	return ui.each(list, function(first: any): Node
		local k, last = key(first), first
		return ui.presence(function(): string
			last = by_key:get()[k] or last
			local ok, said = pcall(ui.json.encode, last)
			return if ok then said else tostring(last)
		end, function(): Node
			return build(last)
		end, { w_full = true })
	end, o)
end

-- `ui.presence`'s options for content that takes another's place (a view
-- for another, a list for its empty state): the old fades, then the new.
function M.swap(opts: { [string]: any }?): { [string]: any }
	local o: { [string]: any } = if opts then table.clone(opts) else {}
	o.enter = motion.transition({ opacity = { 0, 1 }, duration = 120 })
	o.exit = motion.transition({ opacity = 0, duration = 60 })
	o.mode = "wait"
	return o
end

local function holds(on: any): boolean
	local now = if type(on) == "function" then on() elseif type(on) == "boolean" then on else on:get()
	return now and true or false
end

-- Whether `on` holds, kept while what it showed goes: `inside`, what
-- shows (a frame after it starts holding, its room open by then); `away`,
-- its room put away once that has faded and closed (an empty presence
-- still takes its parent's gap; a hidden box takes none). Shown as the
-- view is built, it's simply there.
local function kept(on: any): (any, any)
	local away = ui.use_state(not holds(on))
	local inside = ui.use_state(holds(on))
	ui.effect(function()
		if holds(on) then
			away:set(false)
			ui.after(0, function()
				if holds(on) then
					inside:set(true)
				end
			end)
		else
			inside:set(false)
			ui.after(motion.ms(motion.BASE) + 40, function()
				if not holds(on) then
					away:set(true)
				end
			end)
		end
	end)
	return away, inside
end

local function present(inside: any, node: Node, wide: boolean): Node
	return ui.presence(function(): string?
		return if inside:get() then "on" else nil
	end, function(): Node
		return node
	end, {
		w_full = wide,
		enter = fade_in(),
		exit = motion.transition({ opacity = { 1, 0 }, duration = motion.FADE, ease = motion.EASE }),
	})
end

-- `node` while `on` holds, fading in and out where it stands: a control
-- in a row, beside others that keep their places.
function M.fade(on: any, node: Node): Node
	local away, inside = kept(on)
	return ui.div {
		hidden = away,
		shrink = 0,
		present(inside, node, false),
	}
end

-- `node` while `on` holds (a state, a function or a boolean), as
-- `ui.show`, coming and going in the flow: the room it takes opens and
-- closes as it fades, so what's after it slides rather than jumps (the
-- room is always there while it's used, its height easing, as the checks'
-- body in @ds/git). `box`: the room's own sizing, for content that grows
-- to fill it.
function M.show(on: any, node: Node, box: { [string]: any }?): Node
	local away, inside = kept(on)
	local room: { [any]: any } = {
		w_full = true,
		shrink = 0,
		overflow = "hidden",
		layout = "height",
		layout_clip = true,
		layout_duration = motion.ms(motion.BASE),
		layout_ease = motion.EASE,
		hidden = away,
		present(inside, node, true),
	}
	for k, v in box or {} do
		room[k] = v
	end
	return ui.column(room :: any)
end

return M
