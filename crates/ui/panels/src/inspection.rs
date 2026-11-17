//! The choices of the user for the checks of the project: reading them when a window
//! opens, saving them from Settings, and turning one off from Alt+Enter.

use editor_view::{EditorView, InspectionProfile, Semantic};
use lang_api::InspectionSettings;
use ui_kit::prelude::*;
use workspace::Workspace;

/// Hands `settings` to the language and to the editors that are open. Nothing is
/// written: the caller saves the file.
pub(crate) fn apply(
    workspace: &mut Workspace,
    settings: &InspectionSettings,
    cx: &mut Context<Workspace>,
) {
    cx.set_global(InspectionProfile(settings.clone()));
    if let Some(semantic) = cx.try_global::<Semantic>() {
        semantic.0.set_inspection_settings(settings.clone());
    }
    for (_, view) in workspace.open_editors() {
        if let Ok(editor) = view.downcast::<EditorView>() {
            editor.update(cx, |editor, cx| editor.reapply_inspections(cx));
        }
    }
}

/// Turns `id` off in the project settings, saves them and applies them.
pub(crate) fn disable(
    workspace: &mut Workspace,
    id: &str,
    title: &str,
    cx: &mut Context<Workspace>,
) {
    let root = workspace.root().to_path_buf();
    let mut settings = workspace::load_inspections(&root);
    settings.set(id, false, None);
    let saved = settings.clone();
    let write = cx
        .background_executor()
        .spawn(async move { workspace::save_inspections(&root, &saved) });
    cx.spawn(async move |_, _| {
        if let Err(error) = write.await {
            eprintln!("the inspections of the project are not saved: {error}");
        }
    })
    .detach();
    apply(workspace, &settings, cx);
    workspace.notify_user(
        "Inspection disabled",
        format!("'{title}' is off for this project"),
        cx,
    );
}
