for _, layout in { "legacy", "blocks", "compact" } do
	test(`tool file badges open files without toggling their details ({layout})`, function()
		local app = mount("chat_fixture.luau")
		app:resize(900, 900)
		app:command("go_s2")
		app:frames(30)
		app:command("link_turn")
		app:frames(30)
		local tools = {
			{ id = "read", kind = "read", verb = "Read", target = "lib.rs", path = "src/lib.rs", call = "src/lib.rs", output = "read result" },
			{ id = "edit", kind = "edit", verb = "Edited", target = "a #file.toml", path = "/work/atlas/src/a #file.toml", call = "/work/atlas/src/a #file.toml", output = "edit result" },
			{ id = "unsafe", kind = "read", verb = "Read", target = "secret.rs", path = "../secret.rs", call = "../secret.rs" },
		}
		local group = { id = "tools", kind = "tools", summary = "Tool file parity", tools = tools }
		local message: any = { id = "parity", role = "assistant", text = "", tools = tools }
		if layout ~= "legacy" then
			message.blocks = if layout == "compact"
				then { { id = "work", kind = "work", summary = "Turn work", tools = tools, blocks = { group } } }
				else { group }
		end
		app:set_state("chat.messages", { message })
		app:frames(30)
		app:click(if layout == "legacy" then 'role:button "Read 2 files · edited 1 file"' elseif layout == "compact" then 'role:button "Turn work"' else 'role:button "Tool file parity"')
		app:frames(30)
		if layout == "compact" then
			app:click('role:button "Tool file parity"')
			app:frames(30)
		end
		expect(app:has('role:link "lib.rs"')).to_be(true)
		app:click('role:link "lib.rs"')
		app:frames(10)
		expect(app:state("calls")).to_contain("open_file:s2:src/lib.rs")
		expect(app:has('~"read result"')).to_be(false)
		app:click('role:link "a #file.toml"')
		app:frames(10)
		expect(app:state("calls")).to_contain("open_file:s2:src/a #file.toml")
		expect(app:has('~"edit result"')).to_be(false)
		expect(app:has('role:link "secret.rs"')).to_be(false)
		app:click('role:button "Read lib.rs"')
		app:frames(20)
		expect(app:has('~"read result"')).to_be(true)
		expect(app:error()).to_be_nil()
	end)
end

test("a wide single-line command opens to its full invocation even without output", function()
	local app = mount("chat_fixture.luau")
	app:resize(700, 900)
	app:command("go_s2")
	app:frames(30)
	local command = "printf '" .. string.rep("long-command ", 25) .. "tail_marker'"
	app:set_state("chat.messages", {
		{ id = "wide", role = "assistant", text = "", tools = {
			{ id = "cmd", kind = "exec", verb = "Ran", target = command, call = command },
		} },
	})
	app:frames(30)
	app:click('role:button "Ran 1 command"')
	app:frames(30)
	expect(app:has('.tool-call >> ~"tail_marker"')).to_be(false)
	app:click('role:button ~"Ran printf"')
	app:frames(30)
	expect(app:has('.tool-call >> ~"tail_marker"')).to_be(true)
	expect(app:error()).to_be_nil()
end)
