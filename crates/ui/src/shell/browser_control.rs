use super::*;
use crate::browser::automation::{BrowserWork, success};
use futures::FutureExt;
use serde_json::json;
use zeron_proto::browser::*;
use zeron_rpc::methods;

impl Shell {
    pub(super) fn ensure_browser_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(target_os = "linux") || self.browser_host_task.is_some() {
            return;
        }
        self.browser_host_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let handle = this
                    .read_with(cx, |shell, cx| shell.state.read(cx).engine().cloned())
                    .ok()
                    .flatten();
                if let Some(handle) = handle
                    && handle
                        .engine_info()
                        .supports(zeron_proto::capabilities::BROWSER_CONTROL_V1)
                {
                    let host_id = this
                        .read_with(cx, |shell, _| shell.browser_host_id.clone())
                        .unwrap_or_default();
                    if let Ok(mut stream) = handle
                        .client()
                        .subscribe_scoped(methods::ATTACH_BROWSER_HOST, json!({"hostId":host_id}))
                        .await
                    {
                        while let Some(value) = stream.recv().await {
                            if value["ready"] == true {
                                if this
                                    .update_in(cx, |shell, _, cx| {
                                        shell.browser_host_connected = true;
                                        shell.browser_host_tabs.clear();
                                        shell.publish_browser_tabs(cx);
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                continue;
                            }
                            let Ok(request) = serde_json::from_value::<BrowserHostRequest>(value)
                            else {
                                continue;
                            };
                            let (work, inventory) = match this.update_in(cx, |shell, window, cx| {
                                let work = shell.browser_work(request.request, window, cx);
                                let inventory = json!({"hostId":shell.browser_host_id,"revision":shell.browser_host_revision,"tabs":shell.browser_host_tabs});
                                (work, inventory)
                            }) {
                                Ok(work) => work,
                                Err(_) => return,
                            };
                            let handle = handle.clone();
                            let host_id = host_id.clone();
                            // Native work is Send; a runtime deadline cancels pending
                            // evaluations even if a web process never answers.
                            gpui_tokio::Tokio::spawn(cx, async move {
                                let mut result =
                                    tokio::time::timeout(std::time::Duration::from_secs(18), work)
                                        .await
                                        .unwrap_or_else(|_| {
                                            Err("browser_timeout: page did not respond".into())
                                        });
                                // The next tool call must be able to address a
                                // newly created tab as soon as this reply lands.
                                if let Err(error) = handle.client().call(methods::UPDATE_BROWSER_HOST, inventory).await {
                                    result = Err(format!("browser_no_host: {error}"));
                                }
                                let _ = handle
                                    .client()
                                    .call(
                                        methods::REPLY_BROWSER_HOST,
                                        serde_json::to_value(BrowserHostReply {
                                            host_id,
                                            request_id: request.request_id,
                                            result,
                                        })
                                        .unwrap(),
                                    )
                                    .await;
                            })
                            .detach();
                        }
                    }
                    if this
                        .update_in(cx, |shell, _, _| {
                            shell.browser_host_connected = false;
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                if this.upgrade().is_none() {
                    return;
                }
            }
        }));
    }

    pub(super) fn publish_browser_tabs(&mut self, cx: &mut Context<Self>) {
        if !self.browser_host_connected {
            return;
        }
        let mut tabs = Vec::new();
        for (chat_id, surfaces) in &self.right_tabs {
            for surface in surfaces {
                if let RightSurface::Browser(id) = surface
                    && let Some(browser) = self.browsers.get(id)
                {
                    let browser = browser.read(cx);
                    tabs.push(BrowserTab {
                        id: format!("{}/{id}", self.browser_host_id),
                        chat_id: chat_id.clone(),
                        title: browser.page.title.chars().take(2000).collect(),
                        url: browser.page.url.clone(),
                        loading: browser.page.loading,
                        enabled: browser.agent_control,
                    });
                }
            }
        }
        tabs.sort_by(|a, b| a.id.cmp(&b.id));
        if tabs == self.browser_host_tabs {
            return;
        }
        self.browser_host_tabs = tabs.clone();
        self.browser_host_revision += 1;
        if let Some(handle) = self.state.read(cx).engine().cloned() {
            let params = json!({"hostId":self.browser_host_id,"revision":self.browser_host_revision,"tabs":tabs});
            cx.spawn(async move |_, _| {
                let _ = handle
                    .client()
                    .call(methods::UPDATE_BROWSER_HOST, params)
                    .await;
            })
            .detach();
        }
    }

    fn browser_work(
        &mut self,
        request: BrowserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserWork {
        let fail = |message: &str| futures::future::ready(Err(message.to_owned())).boxed();
        let Some(id) = request
            .browser_id
            .strip_prefix(&format!("{}/", self.browser_host_id))
            .and_then(|id| id.parse::<u64>().ok())
        else {
            return fail("browser_not_found: tab belongs to another desktop");
        };
        if !self
            .right_tabs
            .get(&request.chat_id)
            .is_some_and(|tabs| tabs.contains(&RightSurface::Browser(id)))
        {
            return fail("browser_denied: tab belongs to another chat");
        }
        let Some(browser) = self.browsers.get(&id).cloned() else {
            return fail("browser_closed");
        };
        if !browser.read(cx).agent_control {
            return fail("browser_disabled: agent control was turned off");
        }
        match request.command {
            BrowserAction::NewTab { url } => {
                let Some(id) =
                    self.add_browser_surface_for(request.chat_id, Some(url), false, window, cx)
                else {
                    return fail("browser_not_found");
                };
                self.browsers[&id].update(cx, |browser, _| browser.agent_control = true);
                self.publish_browser_tabs(cx);
                cx.notify();
                futures::future::ready(Ok(
                    json!({"browserId":format!("{}/{id}", self.browser_host_id),"loading":true}),
                ))
                .boxed()
            }
            BrowserAction::Close => {
                if request.chat_id == self.panel_key(cx) {
                    self.close_right_surface(RightSurface::Browser(id), window, cx);
                } else {
                    if let Some(tabs) = self.right_tabs.get_mut(&request.chat_id) {
                        tabs.retain(|s| *s != RightSurface::Browser(id));
                    }
                    self.browsers.remove(&id);
                    self.browser_subs.remove(&id);
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.publish_browser_tabs(cx);
                cx.notify();
                success()
            }
            BrowserAction::Navigate { url } => {
                if let Err(error) = crate::browser::model::transcript_address(&url) {
                    return fail(error);
                }
                browser.update(cx, |browser, cx| {
                    browser.navigate_for_agent(&url, window, cx)
                });
                self.publish_browser_tabs(cx);
                success()
            }
            BrowserAction::Back | BrowserAction::Forward | BrowserAction::Reload => {
                browser.update(cx, |browser, cx| {
                    browser.automate_navigation(request.command, cx)
                });
                success()
            }
            command => browser.read(cx).automate(command),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn background_tabs_preserve_focus_and_consent_is_rechecked_at_execution(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            apply_keymap(
                cx,
                &settings::KeymapConfig::default(),
                settings::ComposerSendBehavior::default(),
            );
        });
        let shell = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: HarnessId::Mock,
                },
                cx,
            );
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.selected_chat = Some("current".into());
            });
            shell.active_chat = "current".into();
            shell
        });
        shell
            .update(cx, |shell, window, cx| {
                let focus = shell.shortcut_focus.clone();
                window.focus(&focus, cx);
                let id = shell
                    .add_browser_surface_for("background".into(), None, false, window, cx)
                    .unwrap();
                assert_eq!(shell.active_chat, "current");
                assert!(focus.is_focused(window));
                let browser_id = format!("{}/{id}", shell.browser_host_id);
                let request = |chat: &str| BrowserRequest {
                    chat_id: chat.into(),
                    browser_id: browser_id.clone(),
                    command: BrowserAction::Snapshot,
                };
                let disabled = shell.browser_work(request("background"), window, cx);
                assert!(
                    futures::executor::block_on(disabled)
                        .unwrap_err()
                        .contains("browser_disabled")
                );
                shell.browsers[&id].update(cx, |browser, _| browser.agent_control = true);
                let other = shell.browser_work(request("current"), window, cx);
                assert!(
                    futures::executor::block_on(other)
                        .unwrap_err()
                        .contains("browser_denied")
                );
                shell.browsers[&id].update(cx, |browser, _| browser.agent_control = false);
                let revoked = shell.browser_work(request("background"), window, cx);
                assert!(
                    futures::executor::block_on(revoked)
                        .unwrap_err()
                        .contains("browser_disabled")
                );
                shell.browsers[&id].update(cx, |browser, _| browser.agent_control = true);
                let mut close = request("background");
                close.command = BrowserAction::Close;
                futures::executor::block_on(shell.browser_work(close, window, cx)).unwrap();
                assert!(!shell.browsers.contains_key(&id));
                assert!(!shell.right_tabs["background"].contains(&RightSurface::Browser(id)));
                assert_eq!(shell.active_chat, "current");
                assert!(focus.is_focused(window));
                window.blur();
                shell
                    .add_browser_surface_for("background".into(), None, false, window, cx)
                    .unwrap();
                assert!(window.focused(cx).is_none());
            })
            .unwrap();
    }
}
