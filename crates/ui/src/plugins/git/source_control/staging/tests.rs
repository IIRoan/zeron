use super::*;
use gpui::{TestAppContext, VisualTestContext};
use serde_json::Value;

struct CachedRows {
    source: Entity<SourceControl>,
    _updates: gpui::Subscription,
    rendered: Rc<RefCell<Vec<FileKey>>>,
}
impl Render for CachedRows {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.source.update(cx, |source, cx| {
            *self.rendered.borrow_mut() = source.visible_file_keys();
            div().size_full().flex().flex_col().children(
                (0..source.rows.len())
                    .map(|row| source.render_row(row, window, cx))
                    .collect::<Vec<_>>(),
            )
        })
    }
}
struct CachedHost(Entity<CachedRows>);
impl Render for CachedHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            self.0
                .clone()
                .cached(gpui::StyleRefinement::default().size_full()),
        )
    }
}

struct Peer {
    runtime: tokio::runtime::Runtime,
    requests: tokio::sync::mpsc::Receiver<String>,
    replies: tokio::sync::mpsc::Sender<String>,
}

impl Peer {
    fn next(&mut self, cx: &mut TestAppContext) -> Value {
        cx.run_until_parked();
        serde_json::from_str(&self.requests.try_recv().expect("expected RPC request")).unwrap()
    }
    fn empty(&mut self, cx: &mut TestAppContext) {
        cx.run_until_parked();
        assert!(self.requests.try_recv().is_err());
    }
    fn answer(&self, request: &Value, result: Result<CheckoutChanges, &str>) {
        let reply = match result {
            Ok(snapshot) => serde_json::json!({"id": request["id"], "ok": snapshot}),
            Err(error) => serde_json::json!({"id": request["id"], "err": error}),
        };
        self.runtime.block_on(async {
            self.replies.send(reply.to_string()).await.unwrap();
            while self.replies.capacity() < self.replies.max_capacity() {
                tokio::task::yield_now().await;
            }
        });
    }
}

fn setup(
    cx: &mut TestAppContext,
) -> (
    Entity<SourceControl>,
    &mut VisualTestContext,
    Peer,
    CheckoutChanges,
) {
    let (view, cx) = super::super::tests::selection_setup(cx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (outbound, requests) = tokio::sync::mpsc::channel(32);
    let (replies, inbound) = tokio::sync::mpsc::channel(32);
    let engine = {
        let _guard = runtime.enter();
        crate::state::EngineHandle::from_test_client(zeron_rpc::RpcClient::new(outbound, inbound))
    };
    let snapshot = view.update(cx, |view, cx| {
        view.state
            .update(cx, |state, _| state.set_test_engine(engine));
        for repo in &mut view.snapshot.as_mut().unwrap().repositories {
            repo.git = Some(zeron_proto::RepositoryGitState {
                head: Some("head-0".into()),
                branch: Some("main".into()),
                ..Default::default()
            });
        }
        view.snapshot.clone().unwrap()
    });
    (
        view,
        cx,
        Peer {
            runtime,
            requests,
            replies,
        },
        snapshot,
    )
}

fn staged_snapshot(
    mut snapshot: CheckoutChanges,
    repository: &str,
    paths: &[&str],
) -> CheckoutChanges {
    for file in &mut snapshot
        .repositories
        .iter_mut()
        .find(|repo| repo.path == repository)
        .unwrap()
        .files
    {
        if paths.contains(&file.path.as_str()) {
            file.index = GitFileState::Modified;
            file.worktree = GitFileState::Unchanged;
        }
    }
    snapshot
}

#[gpui::test]
fn stage_moves_immediately_and_queued_clicks_share_a_request(cx: &mut TestAppContext) {
    let (view, cx, mut peer, baseline) = setup(cx);
    view.update(cx, |view, cx| {
        view.commit_input
            .update(cx, |input, cx| input.set_text("Keep my draft", cx));
        view.select_files(FileKey::new("", "a.rs", false), false, false);
        view.set_staged("".into(), vec!["a.rs".into()], true, cx);
        assert!(view.is_busy());
        assert!(!view.can_commit(cx));
        assert!(
            view.visible_file_keys()
                .contains(&FileKey::new("", "a.rs", true))
        );
        assert!(
            !view
                .visible_file_keys()
                .contains(&FileKey::new("", "a.rs", false))
        );
        assert!(
            view.selected_files
                .contains(&FileKey::new("", "a.rs", true))
        );
        assert!(view.stage_paths_enabled("", &["b.rs".into()], true));
        assert!(!view.stage_paths_enabled("", &["a.rs".into()], false));
    });
    let first = peer.next(cx);
    assert_eq!(first["method"], methods::SET_CHECKOUT_STAGED);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["b.rs".into()], true, cx);
        view.set_staged("".into(), vec!["c.rs".into()], true, cx);
        assert_eq!(view.staged_count(), 3);
        assert_eq!(
            view.staging_queue.as_ref().unwrap().borrow().requests.len(),
            3
        );
    });
    peer.empty(cx);
    peer.answer(&first, Ok(staged_snapshot(baseline.clone(), "", &["a.rs"])));
    let second = peer.next(cx);
    assert_eq!(
        second["params"]["paths"],
        serde_json::json!(["b.rs", "c.rs"])
    );
    assert_eq!(second["params"]["expectedHead"], "head-0");
    assert_eq!(second["params"]["expectedBranch"], "main");
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.staged_count(),
            3,
            "the first reply must keep remaining optimistic moves"
        );
        assert!(view.is_busy());
        assert_eq!(
            view.snapshot.as_ref().unwrap().repositories[1],
            baseline.repositories[1]
        );
    });
    peer.answer(
        &second,
        Ok(staged_snapshot(baseline, "", &["a.rs", "b.rs", "c.rs"])),
    );
    peer.empty(cx);
    view.read_with(cx, |view, cx| {
        assert!(!view.is_busy());
        assert!(view.staging_queue.is_none());
        assert!(view.can_commit(cx));
        assert_eq!(view.commit_input.read(cx).text(), "Keep my draft");
    });
}

#[gpui::test]
fn optimistic_moves_repaint_a_cached_host_before_git_replies(cx: &mut TestAppContext) {
    let (view, cx, mut peer, baseline) = setup(cx);
    let rendered = Rc::new(RefCell::new(Vec::new()));
    cx.update(|window, cx| {
        window.replace_root(cx, |_, cx| {
            CachedHost(cx.new(|cx| {
                let updates = cx.subscribe(&view, |_, _, event, cx| {
                    if matches!(event, SourceControlEvent::Updated) {
                        cx.notify();
                    }
                });
                CachedRows {
                    source: view.clone(),
                    _updates: updates,
                    rendered: rendered.clone(),
                }
            }))
        });
        window.draw(cx).clear();
    });
    assert!(rendered.borrow().contains(&FileKey::new("", "a.rs", false)));
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["a.rs".into()], true, cx)
    });
    let held = peer.next(cx);
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(rendered.borrow().contains(&FileKey::new("", "a.rs", true)));
    assert!(
        !rendered.borrow().contains(&FileKey::new("", "a.rs", false)),
        "the rendered rows must move while the RPC is held back"
    );
    peer.answer(&held, Ok(staged_snapshot(baseline, "", &["a.rs"])));
    peer.empty(cx);
}

#[gpui::test]
fn a_failed_write_rolls_back_and_reconciles_before_the_next_write(cx: &mut TestAppContext) {
    let (view, cx, mut peer, baseline) = setup(cx);
    view.update(cx, |view, cx| {
        view.select_files(FileKey::new("", "a.rs", false), false, false);
        view.set_staged("".into(), vec!["a.rs".into()], true, cx);
    });
    let first = peer.next(cx);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["b.rs".into()], true, cx)
    });
    peer.answer(&first, Err("index.lock exists"));
    let refresh = peer.next(cx);
    assert_eq!(refresh["method"], methods::GET_CHECKOUT_CHANGES);
    view.read_with(cx, |view, _| {
        assert!(
            view.visible_file_keys()
                .contains(&FileKey::new("", "a.rs", false))
        );
        assert!(
            view.selected_files
                .contains(&FileKey::new("", "a.rs", false))
        );
        assert_eq!(view.staging_pending("", "a.rs"), None);
        assert_eq!(view.staging_pending("", "b.rs"), Some(true));
        assert!(view.error.as_deref().unwrap().contains("index.lock"));
    });
    peer.answer(&refresh, Ok(baseline.clone()));
    let next = peer.next(cx);
    assert_eq!(next["params"]["paths"], serde_json::json!(["b.rs"]));
    peer.answer(&next, Ok(staged_snapshot(baseline, "", &["b.rs"])));
    peer.empty(cx);
    view.read_with(cx, |view, _| assert!(!view.is_busy()));
}

#[gpui::test]
fn unavailable_reconciliation_cancels_pending_moves_and_blocks_commit(cx: &mut TestAppContext) {
    let (view, cx, mut peer, baseline) = setup(cx);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["a.rs".into()], true, cx)
    });
    let first = peer.next(cx);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["b.rs".into()], true, cx)
    });
    peer.answer(&first, Err("connection closed"));
    let refresh = peer.next(cx);
    peer.answer(&refresh, Err("offline"));
    peer.empty(cx);
    view.read_with(cx, |view, cx| {
        assert!(!view.is_busy());
        assert!(view.staging_queue.is_none());
        assert_eq!(view.snapshot.as_ref().unwrap(), &baseline);
        assert!(
            view.load_error
                .as_deref()
                .unwrap()
                .contains("verify the index")
        );
        assert!(!view.can_commit(cx));
        assert!(!view.staging_enabled());
    });
}

#[gpui::test]
fn accepted_queue_finishes_on_its_original_checkout_after_navigation(cx: &mut TestAppContext) {
    let (view, cx, mut peer, baseline) = setup(cx);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["a.rs".into()], true, cx)
    });
    let first = peer.next(cx);
    view.update(cx, |view, cx| {
        view.set_staged("".into(), vec!["b.rs".into()], true, cx);
        view.target.as_mut().unwrap().cwd = "/another-checkout".into();
        view.staging_queue = None;
        view.busy = false;
        view.commit_input
            .update(cx, |input, cx| input.set_text("Other checkout draft", cx));
        view.apply_snapshot(baseline.clone(), cx);
    });
    peer.answer(&first, Ok(staged_snapshot(baseline.clone(), "", &["a.rs"])));
    let next = peer.next(cx);
    assert_eq!(next["params"]["cwd"], "/checkout");
    assert_eq!(next["params"]["paths"], serde_json::json!(["b.rs"]));
    peer.answer(
        &next,
        Ok(staged_snapshot(baseline.clone(), "", &["a.rs", "b.rs"])),
    );
    peer.empty(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.snapshot.as_ref().unwrap(), &baseline);
        assert_eq!(view.commit_input.read(cx).text(), "Other checkout draft");
        assert!(!view.is_busy());
    });
}

#[gpui::test]
fn unstaging_moves_immediately_and_authoritative_clean_files_disappear(cx: &mut TestAppContext) {
    let (view, cx, mut peer, mut baseline) = setup(cx);
    view.update(cx, |view, cx| {
        view.select_files(FileKey::new("", "a.rs", true), false, false);
        view.selected = view.file_selection(&FileKey::new("", "a.rs", true));
        view.set_staged("".into(), vec!["a.rs".into()], false, cx);
        assert!(
            !view
                .visible_file_keys()
                .contains(&FileKey::new("", "a.rs", true))
        );
        assert!(
            view.selected_files
                .contains(&FileKey::new("", "a.rs", false))
        );
    });
    let request = peer.next(cx);
    assert_eq!(request["params"]["staged"], false);
    baseline.repositories[0]
        .files
        .retain(|file| file.path != "a.rs");
    peer.answer(&request, Ok(baseline.clone()));
    peer.empty(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.snapshot.as_ref().unwrap(), &baseline);
        assert!(view.selected.is_none());
        assert!(view.selected_files.is_empty());
    });
}

#[test]
fn predictions_preserve_additions_deletions_and_ambiguous_statuses() {
    use GitFileState::*;
    for (index, worktree, staged, expected) in [
        (Untracked, Untracked, true, (Added, Unchanged)),
        (Added, Modified, true, (Added, Unchanged)),
        (Added, Unchanged, false, (Unchanged, Untracked)),
        (Added, Deleted, false, (Unchanged, Unchanged)),
        (Modified, Deleted, true, (Deleted, Unchanged)),
        (Modified, Deleted, false, (Unchanged, Deleted)),
        (Deleted, Unchanged, false, (Unchanged, Deleted)),
        (Renamed, Modified, true, (Renamed, Modified)),
        (Added, Added, true, (Added, Added)),
        (Unmerged, Modified, true, (Unmerged, Modified)),
    ] {
        let mut file = GitFileStatus {
            path: "file".into(),
            old_path: None,
            index,
            worktree,
        };
        project_file(&mut file, staged);
        assert_eq!((file.index, file.worktree), expected);
    }
}

#[gpui::test]
fn queued_submodule_staging_uses_its_own_repository_and_head(cx: &mut TestAppContext) {
    let (view, cx, mut peer, mut baseline) = setup(cx);
    baseline.repositories[1].git.as_mut().unwrap().head = Some("nested-head".into());
    view.update(cx, |view, cx| {
        view.apply_snapshot(baseline.clone(), cx);
        view.set_staged("".into(), vec!["a.rs".into()], true, cx);
    });
    let first = peer.next(cx);
    view.update(cx, |view, cx| {
        view.set_staged("nested".into(), vec!["a.rs".into()], true, cx)
    });
    let root_staged = staged_snapshot(baseline, "", &["a.rs"]);
    peer.answer(&first, Ok(root_staged.clone()));
    let nested = peer.next(cx);
    assert_eq!(nested["params"]["repository"], "nested");
    assert_eq!(nested["params"]["expectedHead"], "nested-head");
    assert_eq!(nested["params"]["paths"], serde_json::json!(["a.rs"]));
    let completed = staged_snapshot(root_staged, "nested", &["a.rs"]);
    peer.answer(&nested, Ok(completed.clone()));
    peer.empty(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.snapshot.as_ref().unwrap(), &completed)
    });
}

#[gpui::test]
fn gitlinks_stay_pending_and_other_files_can_be_staged(cx: &mut TestAppContext) {
    let (view, cx, mut peer, mut baseline) = setup(cx);
    baseline.repositories[0].submodules.push("a.rs".into());
    view.update(cx, |view, cx| {
        view.apply_snapshot(baseline.clone(), cx);
        view.set_staged("".into(), vec!["a.rs".into()], true, cx);
        assert_eq!(view.snapshot.as_ref().unwrap(), &baseline);
        assert_eq!(view.staging_pending("", "a.rs"), Some(true));
        assert!(view.stage_paths_enabled("", &["b.rs".into()], true));
    });
    let first = peer.next(cx);
    peer.answer(&first, Ok(staged_snapshot(baseline, "", &["a.rs"])));
    peer.empty(cx);
}
