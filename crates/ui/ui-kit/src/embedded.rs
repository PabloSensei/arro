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
    gpui_kit::application().with_assets(Assets)
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

/// Decodes every icon up front: the first theme switch must not show empty icons.
fn preload_icons(cx: &mut App) {
    for name in Embedded::iter().filter(|name| name.starts_with("icons/")) {
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
