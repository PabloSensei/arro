# Where `idea-default.json` comes from

| What | From | License |
|---|---|---|
| The IntelliJ IDEA keymap for Windows, `idea-default.json` | `platform/platform-resources/src/keymaps/$default.xml` in `intellij-community` | Apache-2.0: the repository's `LICENSE.txt` (version 1.3 of 2026-06-15) states that the Open-Source Build consists of software under Apache 2.0. The XML file itself has no header |

Source: https://github.com/JetBrains/intellij-community, branch `master`, downloaded 2026-11-03.

Changes made when moving it here, as section 4(b) of Apache-2.0 requires: the file was converted from XML to JSON by `cargo xtask idea-keymap`, keys are written in this crate's syntax (`ctrl-shift-n`, `double-shift`), and mouse shortcuts were dropped. The IDEA action ids are kept unchanged.

To update it, run the same generator:

```
cargo xtask idea-keymap <path to $default.xml> crates/editor/keymap/keymaps/idea-default.json
```
