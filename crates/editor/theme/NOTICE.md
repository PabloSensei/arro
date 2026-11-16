# Where the colors come from

The palette is Arro's own: the window chrome and the code highlighting of the `Dark`,
`Light`, `High Contrast`, `Retro` and `Paper` themes. No values are taken from
`intellij-community` or from an installed IDE.

Two themes follow published palettes:

- **Nord** follows the palette of the Nord project, https://github.com/nordtheme/nord.
  The project publishes its license as MIT. Only the color values are used, written as
  our own token file; no code, icons or text of the project is copied. Where a published
  color did not reach the contrast the tests ask for, the token uses another color of the
  same palette or a lighter shade of it.
  Nord: Copyright (c) 2016-present Sven Greb, MIT License.
- **Mocha** follows the Catppuccin palette, flavor Mocha, https://github.com/catppuccin/catppuccin.
  The project publishes its license as MIT. Only the color values are used, written as
  our own token file; no code, icons or text of the project is copied. `selection` and
  `match_bg` are a shade between two published surface colors, so that code keeps its
  contrast on a selected line.
  Catppuccin: Copyright (c) 2021 Catppuccin, MIT License.

The JetBrains Mono and Inter fonts are under the SIL Open Font License; their license
texts sit next to the font files. The icons are described in `crates/ui/ui-kit/NOTICE.md`.
