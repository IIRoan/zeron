//! Account profile images shared by Cloud settings and project pickers.
use crate::theme::Theme;
use gpui::{AnyElement, SharedString, div, prelude::*, px};

pub(super) fn avatar(login: &str, id: SharedString, size: f32, theme: &Theme) -> AnyElement {
    let initial = login
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    let fallback_theme = theme.clone();
    let fallback = move || {
        div()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(fallback_theme.glass_hover())
            .text_color(fallback_theme.text_muted)
            .text_size(px(size * 0.5))
            .child(initial.clone())
            .into_any_element()
    };
    if login.is_empty() {
        return fallback();
    }
    let mut url = url::Url::parse("https://github.com/").unwrap();
    url.path_segments_mut()
        .unwrap()
        .push(&format!("{login}.png"));
    url.set_query(Some("size=64"));
    div()
        .id(id.clone())
        .size(px(size))
        .flex_none()
        .rounded_full()
        .overflow_hidden()
        .child(
            gpui::img(SharedString::from(url.to_string()))
                .id(id)
                .size(px(size))
                .rounded_full()
                .object_fit(gpui::ObjectFit::Cover)
                .with_loading(fallback.clone())
                .with_fallback(fallback),
        )
        .into_any_element()
}
