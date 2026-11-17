use std::path::PathBuf;
use zgx::scene::{NodeId, NodeKind};
use zgx_script::{ScriptHost, testing::TestApp};

#[test]
fn inline_tool_diffs_keep_syntax_colours_and_scroll_to_the_end_of_wide_lines() {
    let ui = std::env::var_os("ZERON_ZGX_TEST_UI")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    let mut app =
        TestApp::new(|runtime| ScriptHost::new(runtime, ui.join("tool_parity_fixture.luau")));
    app.ui.scene.borrow_mut().resize(360., 400.);
    app.frames(15);
    assert!(app.error().is_none(), "{:?}", app.error());
    let scroll = app.target(".tool-diff-scroll").unwrap();
    let mut stack = vec![scroll];
    let mut code: Option<NodeId> = None;
    {
        let scene = app.ui.scene.borrow();
        while let Some(node) = stack.pop() {
            if let NodeKind::RichText { text } = scene.kind(node) {
                if text.text().contains("tail_marker") {
                    let colours: Vec<_> = text.runs().iter().map(|r| r.color).collect();
                    assert!(
                        colours.iter().any(|c| *c != colours[0]),
                        "code needs syntax colours"
                    );
                    code = Some(node);
                    break;
                }
            }
            stack.extend_from_slice(scene.children(node));
        }
    }
    let code = code.expect("the added code is syntax highlighted");
    let before = app.ui.scene.borrow().bounds(code);
    let viewport = app.ui.scene.borrow().bounds(scroll);
    assert!(before.width > viewport.width * 2., "fixture must overflow");
    zgx_script::driver::wheel(&mut app.ui, scroll, 10000., 0., None);
    app.frames(20);
    let after = app.ui.scene.borrow().bounds(code);
    assert!(
        after.x < before.x - 100.,
        "a horizontal scroll must move the code"
    );
    assert!(
        after.x + after.width <= viewport.x + viewport.width + 1.,
        "the line's end must be reachable"
    );
}
