//! Icons and fonts live in the binary, so a run does not depend on the build folder.

use std::borrow::Cow;

use gpui_kit::*;
use rust_embed::RustEmbed;

use crate::Theme;

#[derive(RustEmbed)]
#[folder = "assets"]
struct Embedded;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(data) = crate::icon_sets::custom_asset(path) {
            return Ok(Some(Cow::Owned(data)));
        }
        Ok(Embedded::get(path).map(|file| file.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Embedded::iter()
            .filter(|name| name.starts_with(path))
            .map(|name| name.to_string().into())
            .collect())
    }
}

/// The application with our assets.
pub fn app() -> Application {
    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos"
    ))]
    let application = Application::with_platform(crate::scale::platform());
    #[cfg(not(any(
        windows,
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos"
    )))]
    let application = gpui_kit::application();
    application.with_assets(Assets)
}

fn load_fonts(cx: &App) {
    let fonts = Embedded::iter()
        .filter(|name| name.ends_with(".ttf"))
        .filter_map(|name| Embedded::get(&name))
        .map(|file| file.data)
        .collect();
    cx.text_system()
        .add_fonts(fonts)
        .expect("embedded fonts load");
}

/// The data of an asset of the binary, for the resolver of the icon sets.
pub(crate) fn embedded_asset(path: &str) -> Option<Cow<'static, [u8]>> {
    Embedded::get(path).map(|file| file.data)
}

/// Adds the file of a set of the settings to the assets of the process.
pub(crate) fn register_asset(path: String, data: Vec<u8>) {
    crate::icon_sets::register_custom_asset(path, data);
}

/// Decodes every icon up front: the first theme switch must not show empty icons.
fn preload_icons(cx: &mut App) {
    for name in Embedded::iter().filter(|name| {
        name.ends_with(".svg") && (name.starts_with("icons/") || name.starts_with("icon-sets/"))
    }) {
        cx.fetch_asset::<ImgResourceLoader>(&Resource::Embedded(name.to_string().into()));
    }
}

/// Fonts, icons and the starting theme. Call once before the first window.
pub fn init(dark: bool, cx: &mut App) {
    load_fonts(cx);
    preload_icons(cx);
    cx.set_global(if dark { Theme::dark() } else { Theme::light() });
}

#[cfg(test)]
mod tests {
    use super::Embedded;

    #[test]
    fn fonts_and_icons_are_inside_the_binary() {
        let fonts = Embedded::iter().filter(|name| name.ends_with(".ttf"));
        assert_eq!(fonts.count(), 6);
        assert!(Embedded::get("icons/general/chevronDown_dark.svg").is_some());
    }
}
