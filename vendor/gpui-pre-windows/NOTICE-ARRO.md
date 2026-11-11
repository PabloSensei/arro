# gpui-pre-windows, modified for Arro

This folder is a copy of the `gpui-pre-windows` 0.3.11 crate from crates.io, the Windows
platform of GPUI by Zed Industries, Inc., licensed under the Apache License 2.0
(`LICENSE-APACHE`). Arro builds against it through `[patch.crates-io]` in the root
`Cargo.toml`.

Arro changed these files; each one says so in its first lines:

| File | Change |
|---|---|
| `src/events.rs` | Keyboard input on any layout: `VK_PACKET`, dead keys, a modifier pressed on its own; the window frame stays drawn by Windows. |
| `src/keyboard.rs` | Shortcuts match on any keyboard layout, not only on US English. |
| `src/platform.rs` | The vsync thread sleeps while no window asks for a frame. |
| `src/vsync.rs` | `FrameDemand`: windows ask for frames, the thread waits for the compositor clock only while they do. |
| `src/window.rs` | `VK_PACKET` input and a frame waker that feeds `FrameDemand`. |

The other files are unchanged apart from line endings.
