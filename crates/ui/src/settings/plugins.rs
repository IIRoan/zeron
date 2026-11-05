//! Settings → Plugins. Bundled native plugins use the same settings store,
//! navigation, search anchors and accessible switches as the other pages.
use gpui::{Context, Window, div, prelude::*, px};

use super::{
    SavePolicy,
    search::{SearchAnchor, SettingId},
    widgets,
};
use crate::{
    plugins::{self, PluginId},
    popover,
    theme::Theme,
};

pub struct PluginsPage {
    scroll: widgets::PageScroll,
    switches: [gpui::FocusHandle; 2],
    _settings: gpui::Subscription,
}

impl PluginsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            switches: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            _settings: cx.observe_global::<super::SettingsStore>(|_, cx| cx.notify()),
        }
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for PluginsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for PluginsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let preferences = super::plugin_preferences(cx);
        let rows = plugins::BUNDLED
            .into_iter()
            .enumerate()
            .map(|(ix, plugin)| {
                let enabled = preferences.enabled(plugin.id);
                let id = plugin.id;
                let (switch, anchor) = match id {
                    PluginId::Git => ("plugin-git-toggle", SettingId::GitPlugin),
                    PluginId::Services => ("plugin-services-toggle", SettingId::ServicesPlugin),
                };
                widgets::card_row(&theme, ix == 0)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(widgets::row_title(&theme, plugin.name))
                            .child(
                                widgets::page_subtitle(&theme, plugin.description)
                                    .px_0()
                                    .mt_0(),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.text_muted)
                                    .child("Included with Zeron"),
                            ),
                    )
                    .child(
                        widgets::toggle_switch(&theme, enabled, switch)
                            .id(switch)
                            .debug_selector(move || switch.into())
                            .track_focus(&self.switches[ix])
                            .cursor_pointer()
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label(format!("{} plugin", plugin.name))
                            .aria_toggled(if enabled {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .focus_visible(|s| s.border_2().border_color(theme.accent).opacity(1.0))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                window.focus(&this.switches[ix], cx);
                                // Read the current value here as another window may have
                                // changed it since this frame was drawn.
                                super::update(SavePolicy::Immediate, cx, |settings| {
                                    let enabled = settings.plugins.enabled(id);
                                    settings.plugins.set_enabled(id, !enabled);
                                });
                                cx.notify();
                            })),
                    )
                    .search_anchor(anchor)
            })
            .collect::<Vec<_>>();
        let scrollbar = popover::rail(self, "plugins-settings-scrollbar", &theme, cx);
        div().id("plugins-settings-host").relative().size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(crate::edge_fade::edge_faded(16.0, true, true,
                div().id("plugins-settings-page").size_full().overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(widgets::page_column()
                        .child(widgets::page_header(&theme, "Plugins", Some(plugins::BUNDLED.len())))
                        .child(widgets::page_subtitle(&theme, "Choose the tools available in your workspace. Changes apply immediately. Running services keep running; saved configuration and drafts are retained."))
                        .child(widgets::section_card(&theme).children(rows))))
                .fade_overflow_y(&self.scroll.scroll))
            .child(widgets::page_reveal(&self.scroll.scroll))
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn mouse_and_keyboard_toggles_persist_without_changing_the_other_plugin(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            super::super::init(Default::default(), dir.path(), cx);
        });
        let (page, cx) = cx.add_window_view(|_, cx| PluginsPage::new(cx));
        cx.simulate_resize(gpui::size(px(900.0), px(700.0)));
        cx.update(|window, cx| window.draw(cx).clear());
        let git = cx.debug_bounds("plugin-git-toggle").unwrap();
        let services = cx.debug_bounds("plugin-services-toggle").unwrap();
        assert!(git.bottom() <= services.top());
        cx.simulate_click(git.center(), gpui::Modifiers::default());
        let saved = super::super::UiSettings::load(dir.path());
        assert!(!saved.plugins.enabled(PluginId::Git));
        assert!(saved.plugins.enabled(PluginId::Services));
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(page.read(cx).switches[0].is_focused(window));
        });
        // The clicked switch keeps focus: Space must take the same path as a click.
        let keystroke = gpui::Keystroke::parse("space").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        let saved = super::super::UiSettings::load(dir.path());
        assert!(saved.plugins.enabled(PluginId::Git));
        cx.simulate_click(services.center(), gpui::Modifiers::default());
        let saved = super::super::UiSettings::load(dir.path());
        assert!(saved.plugins.enabled(PluginId::Git));
        assert!(!saved.plugins.enabled(PluginId::Services));
    }
}
