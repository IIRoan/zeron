//! Repository artwork is fetched once per project/connection, off the render thread.
use super::*;
use crate::files::client::{FilesRequestContext, WorkspaceFilesClient};
use crate::image_media::{MediaImage, decode_project_icon, release_media};

pub(super) const ICON_PATHS: &[&str] = &[
    "public/apple-touch-icon.png",
    "apple-touch-icon.png",
    "public/favicon.svg",
    "favicon.svg",
    "public/favicon.png",
    "public/icon.png",
    "public/logo.png",
    "favicon.png",
    "app/icon.png",
    "src/app/icon.png",
    "public/favicon.ico",
    "favicon.ico",
    "app/favicon.ico",
    "static/favicon.ico",
    "src-tauri/icons/icon.png",
    "assets/icon.png",
    "src/assets/icon.png",
];

fn load_local_icon(root: &std::path::Path) -> Option<MediaImage> {
    // A project can point at a subdirectory; match the remote workspace RPC's
    // checkout-root resolution, including linked worktrees with a .git file.
    let canonical = root.canonicalize().ok()?;
    let root = canonical
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(&canonical);
    for path in ICON_PATHS {
        let path = root.join(path);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::File::open(&path).ok().and_then(|file| {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(zeron_proto::MAX_WORKSPACE_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(bytes)
        })?;
        let mime = if path.extension().is_some_and(|e| e == "svg") {
            "image/svg+xml"
        } else {
            "image/png"
        };
        return decode_project_icon(mime, bytes).ok();
    }
    None
}

// Curated badge tones: (dark appearance, light appearance). Keep the ordering
// stable so projects retain their assigned color. These are explicit colors,
// independent of the selected theme accent; only their appearance variant changes.
const MONOGRAM_PALETTE: [(u32, u32); 8] = [
    (0x94a3b8, 0x475569), // slate
    (0x93c5fd, 0x2563eb), // blue
    (0xc4b5fd, 0x7c3aed), // violet
    (0xfda4af, 0xbe123c), // rose
    (0xfcd34d, 0xa16207), // amber
    (0x6ee7b7, 0x047857), // emerald
    (0x5eead4, 0x0f766e), // teal
    (0xfdba74, 0xc2410c), // orange
];

fn monogram(name: &str, seed: &str, selected: bool, theme: &Theme) -> AnyElement {
    // FNV-1a selects a stable entry from the curated palette.
    let hash = seed.bytes().fold(2166136261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16777619)
    });
    let letter = name
        .trim()
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let (dark, light) = MONOGRAM_PALETTE[hash as usize % MONOGRAM_PALETTE.len()];
    let tone = gpui::Hsla::from(gpui::rgb(if theme.appearance.is_dark() {
        dark
    } else {
        light
    }));
    let mut hover_text = tone;
    hover_text.l = if theme.appearance.is_dark() {
        (tone.l + 0.12).min(0.95)
    } else {
        (tone.l - 0.10).max(0.15)
    };
    let tile = div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        // The active row wears the hover tint permanently.
        .bg(tone.opacity(if selected { 0.24 } else { 0.08 }))
        .text_color(if selected {
            hover_text
        } else {
            tone.opacity(0.85)
        })
        .group_hover("sidebar-session-row", move |style| {
            style.bg(tone.opacity(0.24)).text_color(hover_text)
        })
        .font_family(theme.font_mono.clone())
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(
            div()
                .w_full()
                .text_center()
                .text_size(px(9.0))
                .line_height(px(13.0))
                .child(letter),
        );
    crate::frost::frosted(3.0, crate::frost::MENU_BLUR, tile).into_any_element()
}

pub(super) struct ProjectIcon {
    name: String,
    seed: String,
    media: Option<MediaImage>,
    refreshed: std::time::Instant,
    _task: Task<()>,
}

impl ProjectIcon {
    fn new(
        name: String,
        seed: String,
        context: FilesRequestContext,
        engine: Option<crate::state::EngineHandle>,
        custom: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| release_media(this.media.take(), cx))
            .detach();
        let task = cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let load = async {
                if let Some(path) = custom {
                    if let Ok(media) = executor
                        .spawn(async move { crate::settings::project_icons::load(&path) })
                        .await
                    {
                        return Some(media);
                    }
                }
                if context.target_device_id.is_none() {
                    let root = std::path::PathBuf::from(context.cwd);
                    return executor.spawn(async move { load_local_icon(&root) }).await;
                }
                let client = WorkspaceFilesClient::new(engine?, context.clone());
                for path in ICON_PATHS {
                    // This also resolves the current checkout identity on the owning host.
                    let file = match client
                        .read_file(zeron_proto::ReadWorkspaceFileRequest {
                            target: context.target.clone(),
                            path: (*path).into(),
                        })
                        .await
                    {
                        Ok(file) => file,
                        Err(error) if error.retryable() => return None,
                        Err(_) => continue,
                    };
                    let (mime, bytes) = client
                        .read_image((*path).into(), file.checkout_id)
                        .await
                        .ok()?;
                    return executor
                        .spawn(async move { decode_project_icon(&mime, bytes).ok() })
                        .await;
                }
                None
            };
            let media = match futures::future::select(
                Box::pin(load),
                Box::pin(executor.timer(Duration::from_secs(30))),
            )
            .await
            {
                futures::future::Either::Left((media, _)) => {
                    media.map(|media| media.for_view((16.0, 16.0), 2.0, 4096))
                }
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.media = media;
                cx.notify();
            });
        });
        Self {
            name,
            seed,
            media: None,
            refreshed: std::time::Instant::now(),
            _task: task,
        }
    }
}

impl Render for ProjectIcon {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.media {
            Some(media) => gpui::img(media.image.clone())
                .size_full()
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            None => monogram(&self.name, &self.seed, false, Theme::of(cx)),
        }
    }
}

/// Same card as the pull-request badge tooltip: project name, then the
/// owning device below in the muted line.
struct ProjectIconTooltip {
    name: SharedString,
    device: SharedString,
}

impl Render for ProjectIconTooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .max_w(px(320.0))
            .px(px(9.0))
            .py(px(7.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(crate::popover::surface_bg(theme))
            .when(!theme.is_frost(), |el| el.shadow_md())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .whitespace_nowrap()
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(self.name.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .whitespace_nowrap()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child(self.device.clone()),
            );
        crate::popover::tooltip_surface(card, Some(6.0), window, cx)
    }
}

fn project_icon_frame(
    chat_id: &str,
    name: &str,
    device: &str,
    size: f32,
    child: impl IntoElement,
) -> AnyElement {
    let name: SharedString = name.to_owned().into();
    let device: SharedString = device.to_owned().into();
    div()
        .id(SharedString::from(format!("project-icon-{chat_id}")))
        .size(px(size))
        .flex_none()
        .tooltip(move |_, cx| {
            cx.new(|_| ProjectIconTooltip {
                name: name.clone(),
                device: device.clone(),
            })
            .into()
        })
        .tooltip_show_delay(Duration::from_millis(350))
        .child(child)
        .into_any_element()
}

impl Shell {
    fn project_icon_target(&self, space_id: &str, cx: &App) -> Option<(String, String)> {
        let profile = self.active_sidebar_pin_profile_key(cx)?;
        let space = self.state.read(cx).space_row(space_id)?;
        Some((profile, zeron_proto::view::project_key(space)))
    }

    pub(super) fn has_custom_project_icon(&self, space_id: &str, cx: &App) -> bool {
        self.project_icon_target(space_id, cx)
            .is_some_and(|(profile, project)| {
                crate::settings::project_icons::custom(&profile, &project, cx).is_some()
            })
    }

    pub(super) fn choose_project_icon(&mut self, space_id: String, cx: &mut Context<Self>) {
        let Some((profile, project)) = self.project_icon_target(&space_id, cx) else {
            return;
        };
        let request = crate::settings::project_icons::IconRequest::begin(profile, project, cx);
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose Project Icon".into()),
        });
        let data_dir = self.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.project_icon_error(error.to_string(), cx)
                    });
                    return;
                }
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let valid = this
                .update(cx, |this, cx| {
                    request.is_current(cx)
                        && this.project_icon_target(&space_id, cx)
                            == Some((request.profile.clone(), request.project.clone()))
                })
                .unwrap_or(false);
            if !valid {
                return;
            }
            let prepared = cx
                .background_executor()
                .spawn(async move { crate::settings::project_icons::prepare(&path, &data_dir) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !request.is_current(cx)
                    || this.project_icon_target(&space_id, cx)
                        != Some((request.profile.clone(), request.project.clone()))
                {
                    return;
                }
                let result = prepared.and_then(|prepared| {
                    crate::settings::project_icons::commit(&request, prepared, cx)
                });
                if let Err(error) = result {
                    this.project_icon_error(error, cx);
                }
                this.pull_settings(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn reset_project_icon(&mut self, space_id: &str, cx: &mut Context<Self>) {
        let Some((profile, project)) = self.project_icon_target(space_id, cx) else {
            return;
        };
        let request = crate::settings::project_icons::IconRequest::begin(profile, project, cx);
        if let Err(error) = crate::settings::project_icons::reset(&request, cx) {
            self.project_icon_error(error, cx);
        }
        self.pull_settings(cx);
        cx.notify();
    }

    fn project_icon_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| {
            toasts.error("Project icon".into(), error.into(), cx)
        });
    }

    pub(super) fn render_project_icon_controls(
        &self,
        space_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::settings::widgets::{self, ActionTone};

        let theme = Theme::of(cx).for_popup();
        let space = self.state.read(cx).space_row(space_id).cloned();
        let profile = self.active_sidebar_pin_profile_key(cx);
        let custom = space.as_ref().and_then(|space| {
            crate::settings::project_icons::custom(
                profile.as_deref()?,
                &zeron_proto::view::project_key(space),
                cx,
            )
        });
        let art =
            self.project_icons
                .render(&self.state, space.as_ref(), profile.clone(), false, cx);
        let choose_id = space_id.to_owned();
        let reset_id = space_id.to_owned();
        let enabled = profile.is_some() && space.is_some();
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.0))
            .child(div().size(px(32.0)).flex_none().child(art))
            .child(
                div()
                    .flex_1()
                    .min_w(px(100.0))
                    .child(widgets::row_title(&theme, "Project icon"))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(crate::typography::ui_rems(16.0))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(
                                custom
                                    .as_ref()
                                    .map(|icon| icon.name.clone())
                                    .unwrap_or_else(|| "Automatic".into()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        widgets::action_button(&theme, ActionTone::Quiet, "project-icon-choose")
                            .min_h(px(28.0))
                            .child("Choose image…")
                            .id("project-icon-choose")
                            .debug_selector(|| "project-icon-choose".into())
                            .role(gpui::Role::Button)
                            .aria_label("Choose project icon")
                            .when(!enabled, |el| el.opacity(0.5).cursor_default())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if enabled {
                                    this.choose_project_icon(choose_id.clone(), cx);
                                }
                            })),
                    )
                    .when(custom.is_some(), |el| {
                        el.child(
                            widgets::action_button(&theme, ActionTone::Quiet, "project-icon-reset")
                                .min_h(px(28.0))
                                .child("Reset icon")
                                .id("project-icon-reset")
                                .debug_selector(|| "project-icon-reset".into())
                                .role(gpui::Role::Button)
                                .aria_label("Reset project icon")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.reset_project_icon(&reset_id, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_project_icon(
        &self,
        chat_id: &str,
        size: f32,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id);
        // The tooltip names this row's own project; the artwork is shared.
        let name = chat
            .and_then(|chat| state.space_for_chat(chat))
            .map(|space| space.display_name().to_string())
            .unwrap_or_else(|| "No project".into());
        // Same fallback as the row's "@ device" fragment.
        let device = chat
            .and_then(|chat| state.device_name(&chat.device_id))
            .unwrap_or("Unknown device")
            .to_string();
        let art = self.project_icon_art(chat_id, selected, cx);
        project_icon_frame(chat_id, &name, &device, size, art)
    }

    /// A project group header's icon: the same artwork its rows would wear,
    /// without the tooltip — the label beside it already names the project.
    pub(super) fn render_project_group_icon(
        &self,
        chat_id: &str,
        size: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .size(px(size))
            .flex_none()
            .child(self.project_icon_art(chat_id, false, cx))
            .into_any_element()
    }

    /// Artwork for a chat's project, shared by every space of the project:
    /// the representative's name and color, read from this device's checkout
    /// when there is one (no round trip), else from the representative.
    fn project_icon_art(
        &self,
        chat_id: &str,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let space = state
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| state.space_for_chat(chat))
            .cloned();
        self.project_icons.render(
            &self.state,
            space.as_ref(),
            self.active_sidebar_pin_profile_key(cx),
            selected,
            cx,
        )
    }
}

/// Shared artwork pipeline for sidebar and repository picker. Each owner keeps
/// the entities alive; loading stays off the render thread and has a deadline.
#[derive(Default)]
pub(crate) struct ProjectIcons {
    entries: std::cell::RefCell<std::collections::HashMap<String, Entity<ProjectIcon>>>,
}

impl ProjectIcons {
    pub(crate) fn render<V: 'static>(
        &self,
        state: &Entity<AppState>,
        space: Option<&zeron_proto::Space>,
        profile: Option<String>,
        selected: bool,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let state = state.read(cx);
        let representative = space.map(|space| state.representative_space(space));
        let name = representative
            .map(|space| space.display_name().to_string())
            .unwrap_or_else(|| "Home".into());
        let seed = representative
            .map(|space| space.path.clone())
            .unwrap_or_else(|| "home".into());
        let artwork = space.map(|space| {
            state
                .project_members(space)
                .into_iter()
                .find(|member| state.local_device_id.as_deref() == Some(&member.device_id))
                .unwrap_or_else(|| state.representative_space(space))
        });
        let context = artwork.map(|space| FilesRequestContext {
            target: zeron_proto::WorkspaceTarget {
                chat_id: None,
                space_id: Some(space.id.clone()),
                checkout_path: None,
            },
            target_device_id: (state.local_device_id.as_deref() != Some(&space.device_id))
                .then(|| space.device_id.clone()),
            cwd: space.path.clone(),
            checkout_id: space.checkout_id.clone(),
        });
        let Some(context) = context else {
            return monogram(&name, &seed, selected, Theme::of(cx));
        };
        let custom = profile.as_deref().zip(space).and_then(|(profile, space)| {
            crate::settings::project_icons::custom(
                profile,
                &zeron_proto::view::project_key(space),
                cx,
            )
        });
        let key = format!(
            "{:?}:{:?}:{}:{:?}:{}:{:?}",
            profile,
            context.target_device_id,
            context.cwd,
            context.checkout_id,
            name,
            custom.as_ref().map(|icon| &icon.path)
        );
        let engine = state.engine().cloned();
        // Don't cache a remote miss before a connection exists.
        if custom.is_none() && context.target_device_id.is_some() && engine.is_none() {
            return monogram(&name, &seed, selected, Theme::of(cx));
        }
        let mut cache = self.entries.borrow_mut();
        cache.retain(|_, entity| entity.read(cx).refreshed.elapsed() < Duration::from_secs(300));
        let entity = cache
            .entry(key)
            .or_insert_with(|| {
                let entity = cx.new(|cx| {
                    ProjectIcon::new(
                        name.clone(),
                        seed.clone(),
                        context,
                        engine,
                        custom.map(|icon| icon.path),
                        cx,
                    )
                });
                // The owner draws the selected monogram and redraws when artwork lands.
                cx.observe(&entity, |_, _, cx| cx.notify()).detach();
                entity
            })
            .clone();
        drop(cache);
        if entity.read(cx).media.is_none() {
            return monogram(&name, &seed, selected, Theme::of(cx));
        }
        entity.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png(path: &std::path::Path, width: u32) {
        image::RgbaImage::from_pixel(width, 2, image::Rgba([40, 120, 220, 255]))
            .save(path)
            .unwrap();
    }

    struct IconControlsHost {
        shell: Entity<Shell>,
        _observation: Subscription,
    }

    impl Render for IconControlsHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.shell.update(cx, |shell, cx| {
                div()
                    .size_full()
                    .relative()
                    .p(px(24.0))
                    .child(shell.render_project_icon_controls("project", cx))
                    .children(shell.render_space_overlays(window.viewport_size(), window, cx))
            })
        }
    }

    fn icon_controls_shell(data: &std::path::Path, cx: &mut Context<Shell>) -> Shell {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.local_device_id = Some("local".into());
            // Artwork can be chosen for a disconnected project's remote clones.
            state.spaces = ["project", "clone"]
                .into_iter()
                .map(|id| {
                    serde_json::from_value(serde_json::json!({
                        "id":id, "deviceId":"remote", "path":"/offline/project",
                        "name":"Project", "gitDetected":true, "createdAt":Utc::now(),
                        "repositoryId":"shared",
                    }))
                    .unwrap()
                })
                .collect();
            state
        });
        Shell::new(
            state,
            EngineBootConfig {
                data_dir: data.into(),
                ipc_port: 0,
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: zeron_proto::HarnessId::Mock,
            },
            cx,
        )
    }

    fn init_icon_controls(data: &std::path::Path, cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::settings::init(UiSettings::default(), data, cx);
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            motion::set_reduced_motion(cx, true);
        });
    }

    fn click_icon_control(cx: &mut gpui::VisualTestContext, id: &'static str) {
        let position = cx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("Missing project icon control: {id}"))
            .center();
        cx.simulate_click(position, gpui::Modifiers::default());
    }

    #[gpui::test]
    fn project_icon_picker_repaints_offline_clones_replacements_and_reset(
        cx: &mut gpui::TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("chosen.png");
        let replacement = temp.path().join("replacement.png");
        png(&source, 3);
        png(&replacement, 7);
        init_icon_controls(temp.path(), cx);
        let (host, cx) = cx.add_window_view(|_, cx| {
            let shell = cx.new(|cx| icon_controls_shell(temp.path(), cx));
            let observation = cx.observe(&shell, |_, _, cx| cx.notify());
            IconControlsHost {
                shell,
                _observation: observation,
            }
        });
        let shell = cx.update(|_, cx| host.read(cx).shell.clone());
        cx.refresh().unwrap();
        click_icon_control(cx, "project-icon-choose");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            None
        });
        cx.run_until_parked();
        assert!(cx.update(|_, cx| {
            crate::settings::project_icons::custom("local", "repo:shared", cx).is_none()
        }));
        cx.refresh().unwrap();
        click_icon_control(cx, "project-icon-choose");
        cx.simulate_path_prompt_response(move |_| Some(vec![source]));
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.run_until_parked();
        shell.update(cx, |shell, cx| {
            assert!(shell.has_custom_project_icon("clone", cx));
            let cache = shell.project_icons.entries.borrow();
            assert_eq!(cache.len(), 1);
            assert_eq!(
                cache
                    .values()
                    .next()
                    .unwrap()
                    .read(cx)
                    .media
                    .as_ref()
                    .unwrap()
                    .width,
                3.0
            );
            drop(cache);
            shell
                .space_menu
                .open(("project".into(), gpui::point(px(80.0), px(80.0))));
            cx.notify();
        });
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("space-menu-icon-reset").is_some());
        click_icon_control(cx, "space-menu-icon");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(move |_| Some(vec![replacement]));
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.run_until_parked();
        let path = cx.update(|_, cx| {
            crate::settings::project_icons::custom("local", "repo:shared", cx)
                .unwrap()
                .path
        });
        shell.update(cx, |shell, cx| {
            let cache = shell.project_icons.entries.borrow();
            let (_, icon) = cache
                .iter()
                .find(|(key, _)| key.contains(&format!("{:?}", Some(&path))))
                .unwrap();
            assert_eq!(icon.read(cx).media.as_ref().unwrap().width, 7.0);
        });
        // The real file picker outlives the menu's wall-clock exit animation.
        // This host draws overlays directly, so finish that phase explicitly.
        std::thread::sleep(
            motion::MENU_OUT.total().mul_f32(motion::speed_scale()) + Duration::from_millis(20),
        );
        cx.background_executor.advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        shell.update(cx, |shell, cx| {
            shell.space_menu.finish_close();
            cx.notify();
        });
        cx.refresh().unwrap();
        assert!(cx.update(|_, cx| shell.read(cx).space_menu.get().is_none()));
        click_icon_control(cx, "project-icon-reset");
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.update(|_, cx| {
            crate::settings::project_icons::custom("local", "repo:shared", cx).is_none()
        }));
        assert!(!path.exists());
        assert!(cx.debug_bounds("project-icon-reset").is_none());
    }

    #[gpui::test]
    fn project_icon_picker_does_not_apply_after_workspace_changes(cx: &mut gpui::TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("chosen.png");
        png(&source, 3);
        init_icon_controls(temp.path(), cx);
        let (shell, cx) = cx.add_window_view(|_, cx| icon_controls_shell(temp.path(), cx));
        shell.update(cx, |shell, cx| {
            shell.choose_project_icon("project".into(), cx)
        });
        assert!(cx.did_prompt_for_paths());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.workspace_scope = Some(WorkspaceScope::Synced);
                state.auth = None;
                cx.notify();
            })
        });
        cx.simulate_path_prompt_response(move |_| Some(vec![source]));
        cx.run_until_parked();
        assert!(cx.update(|_, cx| {
            crate::settings::current(cx)
                .project_icons_by_profile
                .is_empty()
        }));
        assert!(!temp.path().join("project-icons").exists());
    }
    #[test]
    fn sidebar_project_icon_priority_and_missing_fallback() {
        let temp = tempfile::tempdir().unwrap();
        // Keep repository discovery inside the fixture, independent of
        // Git markers in the host's temporary directory ancestors.
        std::fs::write(temp.path().join(".git"), "gitdir: /unused-test-checkout").unwrap();
        assert!(load_local_icon(temp.path()).is_none());
        png(&temp.path().join("favicon.png"), 3);
        assert_eq!(load_local_icon(temp.path()).unwrap().width, 3.0);
        std::fs::create_dir(temp.path().join("public")).unwrap();
        png(&temp.path().join("public/apple-touch-icon.png"), 7);
        assert_eq!(load_local_icon(temp.path()).unwrap().width, 7.0);
        std::fs::write(temp.path().join(".git"), "gitdir: /unused-test-checkout").unwrap();
        std::fs::create_dir_all(temp.path().join("src/nested")).unwrap();
        assert_eq!(
            load_local_icon(&temp.path().join("src/nested"))
                .unwrap()
                .width,
            7.0
        );
        // A corrupt first match uses the default rather than showing unrelated lower-priority art.
        std::fs::write(temp.path().join("public/apple-touch-icon.png"), b"invalid").unwrap();
        assert!(load_local_icon(temp.path()).is_none());
    }
    #[test]
    fn sidebar_project_icons_support_svg_and_ico() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".git"), "gitdir: /unused-test-checkout").unwrap();
        std::fs::write(temp.path().join("favicon.svg"), br##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><rect width="12" height="8" fill="#f00"/></svg>"##).unwrap();
        assert_eq!(load_local_icon(temp.path()).unwrap().width, 12.0);
        std::fs::remove_file(temp.path().join("favicon.svg")).unwrap();
        image::RgbaImage::new(16, 16)
            .save(temp.path().join("favicon.ico"))
            .unwrap();
        assert_eq!(load_local_icon(temp.path()).unwrap().width, 16.0);
    }
}
