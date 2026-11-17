# Where `idea-default.json` comes from

| What | From | License |
|---|---|---|
| The IntelliJ IDEA keymap for Windows, `idea-default.json` | `platform/platform-resources/src/keymaps/$default.xml` in `intellij-community` | Apache-2.0: the repository's `LICENSE.txt` (version 1.3 of 2026-06-15) states that the Open-Source Build consists of software under Apache 2.0. The XML file itself has no header |
| The macOS keymap, `idea-default-macos.json` | The entries of [`macos.rs`](../src/macos.rs) on top of `idea-default.json` | The same |

Source: https://github.com/JetBrains/intellij-community, branch `master`, downloaded 2026-11-03.

Changes made when moving it here, as section 4(b) of Apache-2.0 requires: the file was converted from XML to JSON by `cargo xtask idea-keymap`, keys are written in this crate's syntax (`ctrl-shift-n`, `double-shift`), and mouse shortcuts were dropped. The IDEA action ids are kept unchanged.

To update it, run the same generator:

```
cargo xtask idea-keymap <path to $default.xml> crates/editor/keymap/keymaps/idea-default.json
```

## The macOS file

IDEA ships a keymap per system: on macOS the primary modifier is Command and IDEA keeps
Control for the actions it keeps it for. `idea-default-macos.json` therefore has one entry
per action of `idea-default.json`, with Control replaced by Command except for the actions
listed in `src/macos.rs`. That list is IDEA's macOS table for the actions Arro implements,
written from IDEA's documented macOS keymap; the same file writes the mapping, so the table
and the rules cannot disagree. The keys `Insert` and `Menu`, which an Apple keyboard has no
key for, leave an action without a key, as does `MaximizeToolWindow`, whose window button is
the green one on macOS.

To regenerate it after a change of the rules or of the Windows file:

```
cargo xtask macos-keymap crates/editor/keymap/keymaps/idea-default.json crates/editor/keymap/keymaps/idea-default-macos.json
```

