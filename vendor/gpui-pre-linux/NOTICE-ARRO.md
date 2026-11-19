# gpui-pre-linux, modified for Arro

This folder is a copy of the `gpui-pre-linux` 0.3.11 crate from crates.io, the Linux
platform of GPUI by Zed Industries, Inc., licensed under the Apache License 2.0
(`LICENSE-APACHE`). Arro builds against it through `[patch.crates-io]` in the root
`Cargo.toml`.

Arro changed these files; each one says so in its first lines:

| File | Change |
|---|---|
| `src/linux/platform.rs` | The platform carries the interface scale, applies it to the connection and every window, and `LinuxPlatform` is public so the application can set it. |
| `src/linux/display_connection.rs` | The interface scale reaches the windowing backend, or the headless windows, of the current connection. |
| `src/linux/x11/client.rs` | The client keeps the scale of the display apart from the interface scale and applies both to the coordinates of the pointer, the displays and the new windows. |
| `src/linux/x11/window.rs` | The scale factor of a window is the display scale times the interface scale, and a change of the interface scale keeps the size of the window on screen. |
| `src/linux/headless.rs`, `src/linux/headless/window.rs` | Headless windows report the interface scale too. |
| `src/linux/wayland/client.rs` | The client carries the interface scale, converts the coordinates of the pointer from surface units to logical pixels, and opens windows at it. |
| `src/linux/wayland/window.rs` | Surface coordinates and sizes are converted to and from logical pixels, the scale factor is the compositor scale times the interface scale, and a change of the interface scale keeps the window size on screen. |
| `src/gpui_linux.rs` | `LinuxPlatform` is exported. |

The other files are unchanged apart from line endings.
