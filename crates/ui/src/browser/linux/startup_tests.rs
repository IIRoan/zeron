//! A gated helper exercises the actual navigation path without a GTK display.
use super::*;
use crate::browser::{BrowserContext, BrowserSurface};
use gpui::TestAppContext;
use std::{
    path::Path,
    time::{Duration, Instant},
};

struct DelayedHelper(tempfile::TempDir);

impl DelayedHelper {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("helper.py");
        std::fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json, pathlib, struct, sys, time
root = pathlib.Path(__file__).parent
def packet(kind, page, value):
    data = value.encode()
    sys.stdout.buffer.write(struct.pack('<cII', kind, page, len(data)) + data)
    sys.stdout.buffer.flush()
(root / 'started').touch()
while not (root / 'release').exists():
    if not root.exists():
        sys.exit(1)
    time.sleep(.005)
if (root / 'fail').exists():
    packet(b'E', 0, 'Synthetic storage initialization failure')
    sys.exit(1)
packet(b'R', 0, '')
while True:
    header = sys.stdin.buffer.read(4)
    if not header:
        break
    message = json.loads(sys.stdin.buffer.read(struct.unpack('<I', header)[0]))
    if not root.exists():
        break
    with (root / 'commands').open('a') as log:
        log.write(json.dumps(message) + '\n')
    if message['cmd'] == 'close':
        (root / 'closed').touch()
    if message['cmd'] == 'load':
        packet(b'S', message['id'], json.dumps(dict(url=message['url'], title='Loaded',
            loading=False, can_back=False, can_forward=False, error=None)))
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn context(&self) -> BrowserContext {
        BrowserContext {
            data: BrowserData {
                helper_override: Some(self.path().join("helper.py")),
                ..Default::default()
            },
        }
    }

    fn release(&self) {
        std::fs::write(self.path().join("release"), "").unwrap();
    }

    fn loaded_urls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path().join("commands"))
            .unwrap()
            .lines()
            .filter_map(|line| {
                let command: Value = serde_json::from_str(line).unwrap();
                (command["cmd"] == "load").then(|| command["url"].as_str().unwrap().to_owned())
            })
            .collect()
    }
}

fn init(cx: &mut TestAppContext) {
    // These regressions intentionally use real process I/O and external wakes.
    cx.background_executor.allow_parking();
    cx.update(|cx| {
        gpui_base::init(cx);
        crate::composer::init(cx, Default::default());
        cx.set_global(crate::theme::Theme::default());
    });
}

#[test]
fn helper_shutdown_cancels_pending_automation() {
    let (tx, mut events) = tokio::sync::mpsc::channel(4);
    let (reply, mut result) = tokio::sync::oneshot::channel();
    let route = Arc::new(Route {
        notification_pending: AtomicBool::new(false),
        pending: Mutex::new(HashMap::from([("request".into(), reply)])),
        tx,
        state: Mutex::default(),
        frame: Mutex::default(),
        surface: Mutex::default(),
        input: Mutex::new(json!({})),
        #[cfg(feature = "browser-fixture")]
        evaluation: Mutex::default(),
    });
    let routes = Mutex::new(HashMap::from([(1, Arc::downgrade(&route))]));

    helper_stopped(&routes, Engine::Wpe);

    assert!(matches!(
        result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert!(route.state.lock().unwrap().error.is_some());
    assert!(matches!(events.try_recv(), Ok(NativeEvent::Changed)));
}

#[test]
fn failed_gpu_startup_releases_the_profile_before_starting_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, ready: bool| {
        let path = dir.path().join(name);
        let packet = if ready { "b'R', 0, 0" } else { "b'E', 0, 6" };
        let body = if ready { "b''" } else { "b'no GPU'" };
        std::fs::write(
            &path,
            format!(
                r#"#!/usr/bin/env python3
import fcntl, pathlib, struct, sys, time
profile = pathlib.Path(sys.argv[2])
lock = (profile / 'profile.lock').open('a')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
sys.stdout.buffer.write(struct.pack('<cII', {packet}) + {body})
sys.stdout.buffer.flush()
{finish}
"#,
                finish = if ready {
                    "sys.stdin.buffer.read()"
                } else {
                    "time.sleep(60)"
                }
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    let gpu = write("gpu.py", false);
    let fallback = write("software.py", true);
    assert!(spawn_gtk(&gpu, Some(dir.path())).is_err());
    let worker = spawn_gtk(&fallback, Some(dir.path())).unwrap();
    drop(worker);

}

#[test]
fn stalled_helper_input_does_not_block_the_sender_and_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("helper.py");
    std::fs::write(
        &executable,
        r#"#!/usr/bin/env python3
import struct, sys, time
sys.stdout.buffer.write(struct.pack('<cII', b'R', 0, 0))
sys.stdout.buffer.flush()
time.sleep(60)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let data = BrowserData {
        helper_override: Some(executable),
        ..Default::default()
    };
    let worker = data.worker(None).unwrap();
    let started = Instant::now();
    let mut bounded = false;
    for _ in 0..300 {
        if worker
            .send(1, json!({"cmd":"text","text":"x".repeat(16000)}))
            .is_err()
        {
            bounded = true;
            break;
        }
    }
    assert!(
        bounded,
        "hung helpers must not accumulate unbounded commands"
    );
    assert!(
        worker.send(1, json!({"cmd":"close"})).is_ok(),
        "closing a tab must survive input backpressure"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "input blocked the caller"
    );
    let _ = worker.child.lock().unwrap().kill();
}

fn until(cx: &mut TestAppContext, mut condition: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if condition(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "helper/UI completion timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn delayed_startup_keeps_both_windows_responsive_and_loads_latest_address(cx: &mut TestAppContext) {
    init(cx);
    let helper = DelayedHelper::new();
    let context = helper.context();
    let first = cx.add_window(|window, cx| BrowserSurface::new(context.clone(), false, window, cx));
    let second = cx.add_window(|window, cx| BrowserSurface::new(context, false, window, cx));
    let before = Instant::now();
    first
        .update(cx, |browser, window, cx| {
            browser.navigate("https://first.test/", window, cx)
        })
        .unwrap();
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "navigation blocked on helper startup"
    );
    until(cx, |_| helper.path().join("started").exists());

    // The helper is still gated and holds the shared profile's worker lock.
    // A second window must not wait for that lock on the UI thread either.
    let before = Instant::now();
    second
        .update(cx, |browser, window, cx| {
            browser.navigate("https://second.test/", window, cx)
        })
        .unwrap();
    first
        .update(cx, |browser, window, cx| {
            assert!(browser.native.is_none());
            assert!(browser.native_startup.is_some());
            assert!(browser.page.loading);
            browser.navigate("https://latest.test/", window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "another window blocked during startup"
    );

    helper.release();
    until(cx, |cx| {
        first
            .update(cx, |b, _, _| b.page.title == "Loaded")
            .unwrap()
            && second
                .update(cx, |b, _, _| b.page.title == "Loaded")
                .unwrap()
    });
    let mut urls = helper.loaded_urls();
    urls.sort();
    assert_eq!(urls, ["https://latest.test/", "https://second.test/"]);
    first
        .update(cx, |b, _, _| assert!(b.native_startup.is_none()))
        .unwrap();
}

#[gpui::test]
fn delayed_startup_error_is_delivered_to_the_page_and_can_be_retried(cx: &mut TestAppContext) {
    init(cx);
    let helper = DelayedHelper::new();
    std::fs::write(helper.path().join("fail"), "").unwrap();
    let browser =
        cx.add_window(|window, cx| BrowserSurface::new(helper.context(), false, window, cx));
    browser
        .update(cx, |b, w, cx| b.navigate("https://retry.test/", w, cx))
        .unwrap();
    until(cx, |_| helper.path().join("started").exists());
    browser
        .update(cx, |b, _, _| {
            assert!(b.page.loading);
            assert!(b.page.error.is_none());
        })
        .unwrap();
    helper.release();
    until(cx, |cx| {
        browser
            .update(cx, |b, _, _| b.page.error.is_some())
            .unwrap()
    });
    browser
        .update(cx, |b, _, _| {
            assert!(!b.page.loading);
            assert!(b.native.is_none());
            assert!(b.native_startup.is_none());
            assert!(
                b.page
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("Synthetic storage initialization failure")
            );
        })
        .unwrap();
    std::fs::remove_file(helper.path().join("fail")).unwrap();
    browser
        .update(cx, |b, w, cx| b.navigate("https://retry.test/", w, cx))
        .unwrap();
    until(cx, |cx| {
        browser
            .update(cx, |b, _, _| {
                b.page.title == "Loaded" && b.page.error.is_none()
            })
            .unwrap()
    });
}

#[gpui::test]
fn closing_during_startup_cancels_attachment_without_blocking(cx: &mut TestAppContext) {
    init(cx);
    let helper = DelayedHelper::new();
    let browser =
        cx.add_window(|window, cx| BrowserSurface::new(helper.context(), false, window, cx));
    browser
        .update(cx, |b, w, cx| b.navigate("https://closed.test/", w, cx))
        .unwrap();
    until(cx, |_| helper.path().join("started").exists());
    let before = Instant::now();
    browser.update(cx, |b, _, cx| b.close(cx)).unwrap();
    cx.run_until_parked();
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "closing waited for startup"
    );
    helper.release();
    until(cx, |_| helper.path().join("closed").exists());
    browser
        .update(cx, |b, _, _| {
            assert!(b.native.is_none(), "late completion reopened a closed tab");
            assert!(b.native_startup.is_none());
        })
        .unwrap();
    browser
        .update(cx, |b, w, cx| b.navigate("https://reopened.test/", w, cx))
        .unwrap();
    until(cx, |cx| {
        browser
            .update(cx, |b, _, _| b.page.title == "Loaded")
            .unwrap()
    });
    assert_eq!(helper.loaded_urls(), ["https://reopened.test/"]);
}

#[test]
fn concurrent_profiles_can_extract_the_helper_into_a_cold_cache() {
    let root = tempfile::tempdir().unwrap();
    let barrier = std::sync::Barrier::new(8);
    let paths = std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    extract_helper(root.path(), "webkit", GTK_HELPER).unwrap()
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(paths.iter().all(|path| path == &paths[0]));
    assert_eq!(
        std::fs::read(&paths[0]).unwrap(),
        include_bytes!(concat!(env!("OUT_DIR"), "/zeron-webkit"))
    );
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        1,
        "staging files were left behind"
    );
}
