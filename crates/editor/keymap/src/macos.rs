//! The macOS table of the IntelliJ IDEA keys, kept beside the Windows one.
//!
//! IDEA ships a table per system. The macOS one moves the primary modifier from Control to
//! Command and keeps Control where the system or IDEA keeps it: completion, hierarchies,
//! the find-usages keys of the editor, and the refactorings under Control+Option. The
//! entries of [`OVERRIDES`] are that table; every other action takes the Windows keys with
//! Control replaced by Command, which is what IDEA does for the rest.
//!
//! `cargo xtask macos-keymap` writes `keymaps/idea-default-macos.json` from this list, so
//! the file and the rules cannot disagree.

use std::collections::{BTreeMap, HashMap};

use crate::{Keystroke, Shortcut};

/// The keys IDEA gives an action on macOS. An action with no entry keeps the Windows keys
/// with Control replaced by Command; an entry with no keys means macOS has no key for it.
pub const OVERRIDES: &[(&str, &[&str])] = &[
    ("$Copy", &["cmd-c"]),
    ("$Cut", &["cmd-x"]),
    ("$Paste", &["cmd-v"]),
    ("$Redo", &["cmd-shift-z"]),
    ("$SelectAll", &["cmd-a"]),
    ("$Undo", &["cmd-z"]),
    // The tool window slots: Command with the digit of the Windows key.
    ("ActivateBookmarksToolWindow", &["cmd-2"]),
    ("ActivateCommitToolWindow", &["cmd-0"]),
    ("ActivateDebugToolWindow", &["cmd-5"]),
    ("ActivateFindToolWindow", &["cmd-3"]),
    ("ActivateProblemsViewToolWindow", &["cmd-6"]),
    ("ActivateProjectToolWindow", &["cmd-1"]),
    ("ActivateRunToolWindow", &["cmd-4"]),
    ("ActivateServicesToolWindow", &["cmd-8"]),
    ("ActivateStructureToolWindow", &["cmd-7"]),
    ("ActivateVersionControlToolWindow", &["cmd-9"]),
    ("AutoIndentLines", &["cmd-alt-i"]),
    ("Back", &["cmd-[", "cmd-alt-left"]),
    ("CallHierarchy", &["ctrl-alt-h"]),
    ("ChangeSignature", &["cmd-f6"]),
    ("ClassNameCompletion", &["ctrl-alt-space"]),
    ("CloseActiveTab", &["cmd-w"]),
    ("CodeCompletion", &["ctrl-space"]),
    ("CodeInspection.OnEditor", &["cmd-alt-shift-i"]),
    ("CollapseAllRegions", &["cmd-shift--"]),
    ("CollapseBlock", &["cmd-shift-."]),
    ("CollapseRegion", &["cmd--"]),
    ("CollapseRegionRecursively", &["cmd-alt--"]),
    ("CollapseSelection", &["cmd-."]),
    ("CommentByBlockComment", &["cmd-alt-/"]),
    ("CommentByLineComment", &["cmd-/"]),
    ("Console.History.Browse", &["ctrl-alt-e"]),
    ("CopyPaths", &["cmd-shift-c"]),
    ("CopyReference", &["cmd-alt-shift-c"]),
    // Run and Debug keep Control on macOS; the Windows Shift+F10 and Shift+F9 do not.
    ("Debug", &["ctrl-d"]),
    ("EditorCodeBlockEnd", &["cmd-alt-]"]),
    ("EditorCodeBlockStart", &["cmd-alt-["]),
    ("EditorCompleteStatement", &["cmd-shift-enter"]),
    ("EditorDeleteLine", &["cmd-backspace"]),
    ("EditorDeleteToWordEnd", &["alt-delete"]),
    ("EditorDeleteToWordStart", &["alt-backspace"]),
    ("EditorDuplicate", &["cmd-d"]),
    ("EditorJoinLines", &["ctrl-shift-j"]),
    ("EditorLineEnd", &["cmd-right"]),
    ("EditorLineEndWithSelection", &["cmd-shift-right"]),
    ("EditorLineStart", &["cmd-left"]),
    ("EditorLineStartWithSelection", &["cmd-shift-left"]),
    ("EditorLookupDown", &["ctrl-down"]),
    ("EditorLookupUp", &["ctrl-up"]),
    ("EditorNextWord", &["alt-right"]),
    ("EditorNextWordWithSelection", &["alt-shift-right"]),
    ("EditorPasteSimple", &["cmd-alt-shift-v"]),
    ("EditorPreviousWord", &["alt-left"]),
    ("EditorPreviousWordWithSelection", &["alt-shift-left"]),
    ("EditorScrollDown", &["ctrl-down"]),
    ("EditorScrollToCenter", &["ctrl-m"]),
    ("EditorScrollUp", &["ctrl-up"]),
    ("EditorSelectWord", &["alt-up"]),
    ("EditorSplitLine", &["cmd-enter"]),
    ("EditorStartNewLineBefore", &["cmd-alt-enter"]),
    ("EditorTextEnd", &["cmd-down"]),
    ("EditorTextEndWithSelection", &["cmd-shift-down"]),
    ("EditorTextStart", &["cmd-up"]),
    ("EditorTextStartWithSelection", &["cmd-shift-up"]),
    ("EditorToggleCase", &["cmd-shift-u"]),
    ("EditorToggleColumnMode", &["cmd-shift-8"]),
    // An Apple keyboard has no Insert key and no Menu key.
    ("EditorToggleInsertState", &[]),
    ("EditorUnSelectWord", &["alt-down"]),
    ("ExpandAllRegions", &["cmd-shift-="]),
    ("ExpandRegion", &["cmd-="]),
    ("ExpandRegionRecursively", &["cmd-alt-="]),
    ("ExtractMethod", &["cmd-alt-m"]),
    ("ExternalJavaDoc", &["shift-f1"]),
    ("FileStructurePopup", &["cmd-f12"]),
    ("Find", &["cmd-f"]),
    ("FindInPath", &["cmd-shift-f"]),
    ("FindNext", &["cmd-g"]),
    ("FindPrevWordAtCaret", &["ctrl-shift-f3"]),
    ("FindPrevious", &["cmd-shift-g"]),
    ("FindUsages", &["alt-f7"]),
    ("FindUsagesInFile", &["ctrl-f7"]),
    ("FindWordAtCaret", &["ctrl-f3"]),
    ("Forward", &["cmd-]", "cmd-alt-right"]),
    ("Generate", &["cmd-n"]),
    ("GotoAction", &["cmd-shift-a"]),
    ("GotoClass", &["cmd-o"]),
    ("GotoCustomRegion", &["cmd-alt-."]),
    ("GotoDeclaration", &["cmd-b"]),
    ("GotoFile", &["cmd-shift-o"]),
    ("GotoImplementation", &["cmd-alt-b"]),
    ("GotoBookmark0", &["ctrl-0"]),
    ("GotoBookmark1", &["ctrl-1"]),
    ("GotoBookmark2", &["ctrl-2"]),
    ("GotoBookmark3", &["ctrl-3"]),
    ("GotoBookmark4", &["ctrl-4"]),
    ("GotoBookmark5", &["ctrl-5"]),
    ("GotoBookmark6", &["ctrl-6"]),
    ("GotoBookmark7", &["ctrl-7"]),
    ("GotoBookmark8", &["ctrl-8"]),
    ("GotoBookmark9", &["ctrl-9"]),
    ("GotoLine", &["cmd-l"]),
    ("GotoSuperMethod", &["cmd-u"]),
    ("GotoSymbol", &["cmd-alt-o"]),
    ("GotoTest", &["cmd-shift-t"]),
    ("GotoTypeDeclaration", &["ctrl-shift-b"]),
    ("HideAllWindows", &["cmd-shift-f12"]),
    ("HighlightUsagesInFile", &["ctrl-shift-f7"]),
    ("ImplementMethods", &["ctrl-i"]),
    ("Inline", &["cmd-alt-n"]),
    ("InsertLiveTemplate", &["cmd-j"]),
    ("IntroduceConstant", &["cmd-alt-c"]),
    ("IntroduceField", &["cmd-alt-f"]),
    ("IntroduceParameter", &["cmd-alt-p"]),
    ("IntroduceVariable", &["cmd-alt-v"]),
    ("JumpToLastChange", &["cmd-shift-backspace"]),
    ("JumpToLastWindow", &["f12"]),
    // The green button of the window maximizes a tool window on macOS.
    ("MaximizeToolWindow", &[]),
    ("MethodDown", &["cmd-alt-down"]),
    ("MethodHierarchy", &["cmd-shift-h"]),
    ("MethodUp", &["cmd-alt-up"]),
    ("MethodOverloadSwitchDown", &["ctrl-down"]),
    ("MethodOverloadSwitchUp", &["ctrl-up"]),
    ("MoveElementLeft", &["cmd-alt-shift-left"]),
    ("MoveElementRight", &["cmd-alt-shift-right"]),
    ("MoveLineDown", &["cmd-shift-down"]),
    ("MoveLineUp", &["cmd-shift-up"]),
    ("MoveStatementDown", &["ctrl-shift-down"]),
    ("MoveStatementUp", &["ctrl-shift-up"]),
    ("NewScratchFile", &["cmd-shift-n"]),
    ("NewElement", &["ctrl-alt-n"]),
    ("NewElementSamePlace", &["ctrl-alt-shift-n"]),
    ("NextEditorTab", &["cmd-shift-]"]),
    ("NextOccurence", &["ctrl-alt-down"]),
    ("OptimizeImports", &["ctrl-alt-o"]),
    ("OverrideMethods", &["ctrl-o"]),
    ("ParameterInfo", &["cmd-p"]),
    ("PasteMultiple", &["cmd-shift-v"]),
    ("PreviousEditorTab", &["cmd-shift-["]),
    ("PreviousOccurence", &["ctrl-alt-up"]),
    ("QuickImplementations", &["ctrl-shift-i"]),
    ("QuickJavaDoc", &["f1"]),
    ("RecentFiles", &["cmd-e"]),
    ("RecentLocations", &["cmd-shift-e"]),
    ("Refactorings.QuickListPopupAction", &["ctrl-alt-shift-t"]),
    ("ReformatCode", &["cmd-alt-l"]),
    ("Replace", &["cmd-r"]),
    ("ReplaceInPath", &["cmd-shift-r"]),
    ("RestoreDefaultLayout", &["shift-f12"]),
    // Resume Program moves off F9: an Apple keyboard has no function-row key of its own,
    // and IDEA puts it under Option+Command.
    ("Resume", &["alt-cmd-r"]),
    ("Run", &["ctrl-r"]),
    ("RunAnything", &["double-ctrl"]),
    ("RunClass", &["ctrl-shift-r"]),
    ("RunInspection", &["ctrl-alt-shift-i"]),
    ("SafeDelete", &["cmd-delete"]),
    ("SaveAll", &["cmd-s"]),
    ("SaveAs", &["cmd-shift-s"]),
    ("SearchEverywhere", &["double-shift"]),
    ("SelectNextOccurrence", &["ctrl-g"]),
    ("SendEOF", &["ctrl-d"]),
    ("ShowBookmarks", &["shift-f3"]),
    ("ShowErrorDescription", &["cmd-f1"]),
    ("ShowFilePath", &["cmd-alt-f12"]),
    ("ShowFilterPopup", &["cmd-alt-f"]),
    ("ShowIntentionActions", &["alt-enter"]),
    ("ShowPopupMenu", &[]),
    ("ShowReformatFileDialog", &["cmd-alt-shift-l"]),
    ("ShowSettingsAndFindUsages", &["ctrl-alt-shift-f7"]),
    ("ShowTypeBookmarks", &["cmd-shift-f3"]),
    ("ShowUsages", &["cmd-alt-f7"]),
    ("SmartTypeCompletion", &["ctrl-shift-space"]),
    ("SurroundWith", &["cmd-alt-t"]),
    ("SurroundWithLiveTemplate", &["cmd-alt-j"]),
    ("Switcher", &["ctrl-tab", "ctrl-shift-tab"]),
    ("SwitcherIterateItems", &[]),
    ("TodoViewGroupByFlattenPackage", &["cmd-alt-c"]),
    ("TodoViewGroupByShowModules", &["cmd-alt-m"]),
    ("TodoViewGroupByShowPackages", &["cmd-alt-p"]),
    ("ToggleBookmark0", &["ctrl-shift-0"]),
    ("ToggleBookmark1", &["ctrl-shift-1"]),
    ("ToggleBookmark2", &["ctrl-shift-2"]),
    ("ToggleBookmark3", &["ctrl-shift-3"]),
    ("ToggleBookmark4", &["ctrl-shift-4"]),
    ("ToggleBookmark5", &["ctrl-shift-5"]),
    ("ToggleBookmark6", &["ctrl-shift-6"]),
    ("ToggleBookmark7", &["ctrl-shift-7"]),
    ("ToggleBookmark8", &["ctrl-shift-8"]),
    ("ToggleBookmark9", &["ctrl-shift-9"]),
    ("ToggleBookmark", &["f3"]),
    ("ToggleBookmarkWithMnemonic", &["alt-f3"]),
    ("ToggleFindInSelection", &["cmd-alt-e"]),
    ("ToggleRenderedDocPresentation", &["cmd-alt-q"]),
    ("TypeHierarchy", &["ctrl-h"]),
    ("UnselectPreviousOccurrence", &["ctrl-shift-g"]),
    ("UsageFiltering.Imports", &["cmd-i"]),
    ("UsageFiltering.ReadAccess", &["cmd-r"]),
    ("UsageFiltering.WriteAccess", &["cmd-w"]),
    ("UsageGrouping.Directory", &["cmd-alt-p"]),
    ("UsageGrouping.FileStructure", &["cmd-alt-f"]),
    ("UsageGrouping.FlattenModules", &["cmd-alt-o"]),
    ("UsageGrouping.Module", &["cmd-alt-m"]),
    ("UsageGrouping.UsageType", &["cmd-alt-t"]),
    ("UsageView.Include", &[]),
    ("Vcs.RollbackChangedLines", &["cmd-alt-z"]),
    ("VcsShowNextChangeMarker", &["cmd-alt-shift-down"]),
    ("VcsShowPrevChangeMarker", &["cmd-alt-shift-up"]),
    ("ViewBreakpoints", &["cmd-shift-f8"]),
    ("XDebugger.AttachToProcess", &["cmd-alt-f5"]),
];

/// The macOS table for the actions of a Windows table. Every action keeps its id: a key of
/// the Windows table that names no key of an Apple keyboard is dropped instead.
pub fn from_windows(
    windows: &HashMap<String, Vec<Shortcut>>,
) -> Result<BTreeMap<String, Vec<Shortcut>>, crate::KeymapError> {
    let mut table = BTreeMap::new();
    for (id, keys) in windows {
        let mac = match OVERRIDES
            .iter()
            .find(|(action, _)| action == id)
            .map(|(_, keys)| *keys)
        {
            Some(keys) => keys
                .iter()
                .map(|key| Shortcut::parse(key))
                .collect::<Result<Vec<_>, _>>()?,
            None => translate(keys),
        };
        table.insert(id.clone(), mac);
    }
    Ok(table)
}

/// The Windows keys with Control replaced by Command. A shortcut that needs a key an Apple
/// keyboard does not have is left out as a whole.
fn translate(keys: &[Shortcut]) -> Vec<Shortcut> {
    let mut translated = Vec::new();
    for shortcut in keys {
        let Shortcut::Keys(strokes) = shortcut else {
            translated.push(shortcut.clone());
            continue;
        };
        let mut out = Vec::with_capacity(strokes.len());
        let mut usable = true;
        for stroke in strokes {
            if matches!(stroke.key.as_str(), "insert" | "menu") {
                usable = false;
                break;
            }
            out.push(Keystroke {
                ctrl: false,
                cmd: stroke.ctrl || stroke.cmd,
                ..stroke.clone()
            });
        }
        if usable && !translated.contains(&Shortcut::Keys(out.clone())) {
            translated.push(Shortcut::Keys(out));
        }
    }
    translated
}
