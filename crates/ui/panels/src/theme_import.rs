//! Import of a colour scheme of the other IDE (`.icls`): the file is read off the window
//! thread and becomes a theme file in the `themes` folder of the settings, over the theme
//! the window shows. The window reads the folder again and takes the new theme.

use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

/// The Settings | Appearance button "Import…": choose a scheme file and turn it into a
/// theme of the settings folder.
pub(crate) fn import(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let settings_dir = workspace.settings_dir().to_path_buf();
    let parent = cx.global::<Theme>().id().to_owned();
    let dark = cx.global::<Theme>().dark;
    let library = ThemeCatalog::library_of(cx);
    let taken: Vec<String> = library
        .themes()
        .iter()
        .map(|theme| theme.id.clone())
        .collect();
    // A scheme of the user may replace its own file; only a built-in id is taken.
    let own: Vec<String> = library
        .themes()
        .iter()
        .filter(|theme| !theme.builtin)
        .map(|theme| theme.id.clone())
        .collect();
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Import a Colour Scheme".into()),
    });
    cx.spawn(async move |workspace, cx| {
        let chosen = paths.await.ok().and_then(Result::ok).flatten();
        let Some(scheme) = chosen.and_then(|paths| paths.into_iter().next()) else {
            return;
        };
        let target = scheme.clone();
        let read = cx
            .background_executor()
            .spawn(async move { std::fs::read_to_string(&target) });
        let text = match read.await {
            Ok(text) => text,
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_error(
                            "Cannot read the colour scheme",
                            format!("{}: {error}", scheme.display()),
                            cx,
                        )
                    })
                    .ok();
                return;
            }
        };
        let file = match from_icls(&text, &parent, dark) {
            Ok(file) => file,
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_error(
                            "Cannot import the colour scheme",
                            error.to_string(),
                            cx,
                        )
                    })
                    .ok();
                return;
            }
        };
        let mut id = slug(&file.name);
        while taken.contains(&id) && !own.contains(&id) {
            id.push_str("-imported");
        }
        let name = file.name.clone();
        let folder = themes_folder(&settings_dir);
        let path = folder.join(format!("{id}.theme.json"));
        let write_folder = folder.clone();
        let written = cx
            .background_executor()
            .spawn(async move {
                std::fs::create_dir_all(&write_folder)?;
                std::fs::write(path, file.to_json())
            })
            .await;
        workspace
            .update(cx, |workspace, cx| match written {
                Ok(()) => {
                    workspace.reload_themes(cx);
                    workspace.choose_theme(&id, cx);
                    workspace.notify_user(
                        "Colour scheme imported",
                        format!("{name} is a theme of the settings folder now."),
                        cx,
                    );
                }
                Err(error) => workspace.notify_error(
                    "Cannot write the theme",
                    format!("{}: {error}", folder.display()),
                    cx,
                ),
            })
            .ok();
    })
    .detach();
}

/// The note the Appearance page shows above the themes.
pub(crate) const THEMES_NOTE: &str = "A theme is a JSON file in the themes folder of the settings. A file over a built-in theme names only its own colours.";
