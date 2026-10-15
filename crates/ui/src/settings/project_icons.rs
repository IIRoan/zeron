//! Managed copies of user-selected project artwork. Publish the saved pointer
//! before retiring its old file, and keep imports off the UI thread.
use super::{SavePolicy, SettingsStore, current, replace};
use crate::image_media::{MediaImage, decode_project_icon};
use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const DIRECTORY: &str = "project-icons";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomProjectIcon {
    pub path: PathBuf,
    pub name: String,
}

#[derive(Default)]
struct Requests(HashMap<(String, String), u64>);
impl Global for Requests {}

/// Shared across windows: a newer choice or reset cancels an older import.
pub(crate) struct IconRequest {
    pub profile: String,
    pub project: String,
    generation: u64,
}

impl IconRequest {
    pub fn begin(profile: String, project: String, cx: &mut App) -> Self {
        if !cx.has_global::<Requests>() {
            cx.set_global(Requests::default());
        }
        let generation = cx
            .global_mut::<Requests>()
            .0
            .entry((profile.clone(), project.clone()))
            .or_default();
        *generation = generation.wrapping_add(1);
        Self {
            profile,
            project,
            generation: *generation,
        }
    }

    pub fn is_current(&self, cx: &App) -> bool {
        cx.try_global::<Requests>().is_some_and(|requests| {
            requests
                .0
                .get(&(self.profile.clone(), self.project.clone()))
                == Some(&self.generation)
        })
    }
}

pub(crate) fn custom(profile: &str, project: &str, cx: &App) -> Option<CustomProjectIcon> {
    cx.try_global::<SettingsStore>()?
        .current
        .project_icons_by_profile
        .get(profile)?
        .get(project)
        .cloned()
}

fn read_image(path: &Path) -> Result<(Vec<u8>, &'static str), String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(zeron_proto::MAX_WORKSPACE_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "Unable to read this image. Choose a readable image file.".to_string())?;
    if bytes.len() > zeron_proto::MAX_WORKSPACE_IMAGE_BYTES {
        return Err(format!(
            "Choose an image smaller than {} MB.",
            zeron_proto::MAX_WORKSPACE_IMAGE_BYTES / 1024 / 1024
        ));
    }
    // Decode the bytes rather than trusting the selected filename's extension.
    let mime = if image::guess_format(&bytes).is_ok() {
        "image/png"
    } else {
        "image/svg+xml"
    };
    Ok((bytes, mime))
}

pub(crate) fn load(path: &Path) -> Result<MediaImage, String> {
    let (bytes, mime) = read_image(path)?;
    decode_project_icon(mime, bytes).map_err(|_| {
        "This image is unsupported, damaged or too large. Choose a PNG, JPEG, WebP, GIF, SVG or ICO image.".into()
    })
}

#[derive(Debug)]
pub(crate) struct PreparedIcon(Option<CustomProjectIcon>);

impl Drop for PreparedIcon {
    fn drop(&mut self) {
        if let Some(icon) = &self.0 {
            let _ = std::fs::remove_file(&icon.path);
        }
    }
}

/// Called on the background executor. Validate the exact bytes being copied.
pub(crate) fn prepare(source: &Path, data_dir: &Path) -> Result<PreparedIcon, String> {
    let (bytes, mime) = read_image(source)?;
    decode_project_icon(mime, bytes.clone()).map_err(|_| {
        "This image is unsupported, damaged or too large. Choose a PNG, JPEG, WebP, GIF, SVG or ICO image.".to_string()
    })?;
    let directory = data_dir.join(DIRECTORY);
    std::fs::create_dir_all(&directory)
        .map_err(|_| "Unable to save the project icon. Check folder permissions.".to_string())?;
    let extension = image::guess_format(&bytes)
        .ok()
        .and_then(|format| format.extensions_str().first().copied())
        .unwrap_or("svg");
    let path = directory.join(format!(
        "project-icon-{}.{}",
        uuid::Uuid::new_v4(),
        extension
    ));
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, bytes)
        .and_then(|_| std::fs::rename(&temporary, &path))
        .is_err()
    {
        let _ = std::fs::remove_file(temporary);
        return Err("Unable to save the project icon. Check folder permissions.".into());
    }
    Ok(PreparedIcon(Some(CustomProjectIcon {
        path,
        name: source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    })))
}

fn retire(previous: Option<CustomProjectIcon>, data_dir: &Path, cx: &App) {
    let Some(previous) = previous else {
        return;
    };
    let directory = data_dir.join(DIRECTORY);
    // Hand-edited settings can refer to arbitrary or shared files.
    if previous.path.parent() == Some(directory.as_path())
        && !current(cx)
            .project_icons_by_profile
            .values()
            .flat_map(|icons| icons.values())
            .any(|icon| icon.path == previous.path)
    {
        let _ = std::fs::remove_file(previous.path);
    }
}

pub(crate) fn commit(
    request: &IconRequest,
    mut prepared: PreparedIcon,
    cx: &mut App,
) -> Result<(), String> {
    if !request.is_current(cx) {
        return Ok(());
    }
    let data_dir = cx
        .try_global::<SettingsStore>()
        .ok_or_else(|| "Unable to save the project icon. Restart Zeron and try again.".to_string())?
        .data_dir
        .clone();
    let icon = prepared.0.as_ref().unwrap().clone();
    let mut next = current(cx);
    let previous = next
        .project_icons_by_profile
        .entry(request.profile.clone())
        .or_default()
        .insert(request.project.clone(), icon);
    next.save(&data_dir)
        .map_err(|_| "Unable to save the project icon. Check folder permissions.".to_string())?;
    prepared.0.take();
    replace(next, SavePolicy::Immediate, cx);
    retire(previous, &data_dir, cx);
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn reset(request: &IconRequest, cx: &mut App) -> Result<(), String> {
    if !request.is_current(cx) {
        return Ok(());
    }
    let data_dir = cx
        .try_global::<SettingsStore>()
        .ok_or_else(|| {
            "Unable to reset the project icon. Restart Zeron and try again.".to_string()
        })?
        .data_dir
        .clone();
    let mut next = current(cx);
    let Some(icons) = next.project_icons_by_profile.get_mut(&request.profile) else {
        return Ok(());
    };
    let Some(previous) = icons.remove(&request.project) else {
        return Ok(());
    };
    if icons.is_empty() {
        next.project_icons_by_profile.remove(&request.profile);
    }
    next.save(&data_dir)
        .map_err(|_| "Unable to reset the project icon. Check folder permissions.".to_string())?;
    replace(next, SavePolicy::Immediate, cx);
    retire(Some(previous), &data_dir, cx);
    cx.refresh_windows();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{UiSettings, init};
    use gpui::TestAppContext;

    fn png(path: &Path) {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(8, 4, image::Rgba([12, 34, 56, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, bytes.into_inner()).unwrap();
    }

    #[gpui::test]
    fn project_icon_copy_survives_original_deletion_and_settings_reload(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("artwork.wrong-extension");
        png(&source);
        let prepared = prepare(&source, &data).unwrap();
        std::fs::remove_file(&source).unwrap();
        cx.update(|cx| {
            init(
                UiSettings {
                    sidebar_width: 333.0,
                    last_space_id: Some("keep-project".into()),
                    ..Default::default()
                },
                &data,
                cx,
            );
            let request = IconRequest::begin("local".into(), "repo:shared".into(), cx);
            commit(&request, prepared, cx).unwrap();
            let saved = UiSettings::load(&data);
            let icon = saved.project_icons_by_profile["local"]["repo:shared"].clone();
            assert!(icon.path.starts_with(data.join(DIRECTORY)));
            assert_eq!(load(&icon.path).unwrap().width, 8.0);
            assert_eq!(icon.name, "artwork.wrong-extension");
            assert_eq!(saved.sidebar_width, 333.0);
            assert_eq!(saved.last_space_id.as_deref(), Some("keep-project"));
            init(saved, &data, cx);
            assert_eq!(custom("local", "repo:shared", cx).unwrap().path, icon.path);
        });
    }

    #[gpui::test]
    fn project_icon_failed_import_and_save_keep_previous_icon(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("icon.png");
        png(&source);
        cx.update(|cx| {
            init(UiSettings::default(), &data, cx);
            let request = IconRequest::begin("local".into(), "project".into(), cx);
            commit(&request, prepare(&source, &data).unwrap(), cx).unwrap();
            let previous = custom("local", "project", cx).unwrap();
            std::fs::write(&source, b"damaged image").unwrap();
            assert!(prepare(&source, &data).is_err());
            assert_eq!(custom("local", "project", cx), Some(previous.clone()));
            png(&source);
            let candidate = prepare(&source, &data).unwrap();
            let candidate_path = candidate.0.as_ref().unwrap().path.clone();
            // A directory at the settings filename forces rename to fail on any host.
            let settings_path = UiSettings::path(&data);
            std::fs::remove_file(&settings_path).unwrap();
            std::fs::create_dir(&settings_path).unwrap();
            assert!(commit(&request, candidate, cx).is_err());
            assert!(!candidate_path.exists());
            assert!(reset(&request, cx).is_err());
            assert_eq!(custom("local", "project", cx), Some(previous.clone()));
            assert!(previous.path.exists());
        });
    }

    #[gpui::test]
    fn project_icon_newer_request_cancels_old_import_and_profiles_stay_separate(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("icon.png");
        png(&source);
        cx.update(|cx| {
            init(UiSettings::default(), &data, cx);
            let stale = IconRequest::begin("local".into(), "project".into(), cx);
            let candidate = prepare(&source, &data).unwrap();
            let discarded_path = candidate.0.as_ref().unwrap().path.clone();
            let current_request = IconRequest::begin("local".into(), "project".into(), cx);
            reset(&current_request, cx).unwrap();
            commit(&stale, candidate, cx).unwrap();
            assert!(!discarded_path.exists());
            assert!(custom("local", "project", cx).is_none());
            let remote = IconRequest::begin("synced:org:user".into(), "project".into(), cx);
            assert!(current_request.is_current(cx));
            commit(&remote, prepare(&source, &data).unwrap(), cx).unwrap();
            commit(&current_request, prepare(&source, &data).unwrap(), cx).unwrap();
            let local_path = custom("local", "project", cx).unwrap().path;
            let remote_path = custom("synced:org:user", "project", cx).unwrap().path;
            assert_ne!(local_path, remote_path);
            reset(
                &IconRequest::begin("local".into(), "project".into(), cx),
                cx,
            )
            .unwrap();
            assert!(!local_path.exists());
            assert!(remote_path.exists());
            assert!(
                UiSettings::load(&data).project_icons_by_profile["synced:org:user"]
                    .contains_key("project")
            );
        });
    }

    #[gpui::test]
    fn project_icon_reset_preserves_shared_managed_files_and_external_files(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("original.png");
        png(&source);
        cx.update(|cx| {
            init(UiSettings::default(), &data, cx);
            let first = IconRequest::begin("local".into(), "first".into(), cx);
            commit(&first, prepare(&source, &data).unwrap(), cx).unwrap();
            let icon = custom("local", "first", cx).unwrap();
            let mut settings = current(cx);
            settings
                .project_icons_by_profile
                .get_mut("local")
                .unwrap()
                .insert("second".into(), icon.clone());
            replace(settings, SavePolicy::Immediate, cx);
            reset(&first, cx).unwrap();
            assert!(icon.path.exists());
            let mut settings = current(cx);
            settings
                .project_icons_by_profile
                .get_mut("local")
                .unwrap()
                .insert(
                    "external".into(),
                    CustomProjectIcon {
                        path: source.clone(),
                        name: "original.png".into(),
                    },
                );
            replace(settings, SavePolicy::Immediate, cx);
            reset(
                &IconRequest::begin("local".into(), "external".into(), cx),
                cx,
            )
            .unwrap();
            assert!(source.exists());
            reset(&IconRequest::begin("local".into(), "second".into(), cx), cx).unwrap();
            assert!(!icon.path.exists());
        });
    }

    #[test]
    fn project_icon_svg_and_byte_limits_use_the_same_decoder_as_rendering() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("image.dat");
        std::fs::write(&source, br##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><rect width="12" height="8" fill="#f00"/></svg>"##).unwrap();
        let prepared = prepare(&source, temp.path()).unwrap();
        assert_eq!(
            load(&prepared.0.as_ref().unwrap().path).unwrap().width,
            12.0
        );
        let file = std::fs::File::create(&source).unwrap();
        file.set_len(zeron_proto::MAX_WORKSPACE_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(prepare(&source, temp.path()).unwrap_err().contains("MB"));
    }
}
