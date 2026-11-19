# gpui-pre-macos, modified for Arro

This folder is a copy of the `gpui-pre-macos` 0.3.11 crate from crates.io, the macOS
platform of GPUI by Zed Industries, Inc., licensed under the Apache License 2.0
(`LICENSE-APACHE`). Arro builds against it through `[patch.crates-io]` in the root
`Cargo.toml`.

Arro changed these files; each one says so in its first lines:

| File | Change |
|---|---|
| `src/platform.rs` | The platform carries the interface scale, applies it to every open window and gives it to the windows it opens. |
| `src/window.rs` | The scale factor of a window is the backing scale of the screen times the interface scale; the bounds, the content size and the coordinates of the pointer are in logical pixels; a change of the interface scale keeps the window size on screen, and a move to another screen keeps the interface scale. |
| `src/events.rs` | Pointer positions of events are converted from screen points to logical pixels with the interface scale. |

The other files are unchanged apart from line endings.
