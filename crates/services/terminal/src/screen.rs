//! What a renderer reads: an immutable picture of the terminal, in plain types of this crate.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellColor {
    /// The default foreground or background of the theme.
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellFlags(u8);

impl CellFlags {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const INVERSE: Self = Self(1 << 3);
    pub const DIM: Self = Self(1 << 4);
    pub const STRIKEOUT: Self = Self(1 << 5);
    /// A wide glyph that takes this cell and the next.
    pub const WIDE: Self = Self(1 << 6);
    /// The right half of a wide glyph, draw nothing here.
    pub const WIDE_SPACER: Self = Self(1 << 7);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: CellColor::Default,
            bg: CellColor::Default,
            flags: CellFlags::default(),
        }
    }
}

/// An OSC 8 hyperlink on one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
    pub uri: String,
}

/// Modes a host acts on. Copied into every snapshot, and kept fresher than snapshots in the
/// session itself, because input has to be encoded for the mode in force right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub win32_input: bool,
    pub bracketed_paste: bool,
    pub app_cursor: bool,
    pub alt_screen: bool,
    pub focus_events: bool,
    pub mouse: bool,
}

impl Modes {
    pub(crate) fn to_bits(self) -> u32 {
        [
            self.win32_input,
            self.bracketed_paste,
            self.app_cursor,
            self.alt_screen,
            self.focus_events,
            self.mouse,
        ]
        .iter()
        .enumerate()
        .fold(0, |bits, (index, on)| bits | (*on as u32) << index)
    }

    pub(crate) fn from_bits(bits: u32) -> Self {
        let on = |index: u32| bits & (1 << index) != 0;
        Self {
            win32_input: on(0),
            bracketed_paste: on(1),
            app_cursor: on(2),
            alt_screen: on(3),
            focus_events: on(4),
            mouse: on(5),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    /// `rows * cols` cells, row after row.
    pub cells: Vec<Cell>,
    /// `(row, col)`, `None` while the cursor is hidden or scrolled out of view.
    pub cursor: Option<(u16, u16)>,
    /// Lines of history above the live screen that are in view.
    pub display_offset: usize,
    pub title: String,
    pub modes: Modes,
    pub links: Vec<Link>,
    pub bells: u64,
    /// Grows with every published change.
    pub version: u64,
    pub exit_code: Option<u32>,
}

impl Screen {
    pub fn row(&self, row: u16) -> &[Cell] {
        let (cols, row) = (self.cols as usize, row as usize);
        &self.cells[row * cols..(row + 1) * cols]
    }

    /// The text of a row without trailing blanks.
    pub fn line(&self, row: u16) -> String {
        let cells = self.row(row).iter();
        let visible = cells.filter(|cell| !cell.flags.contains(CellFlags::WIDE_SPACER));
        let line: String = visible.map(|cell| cell.ch).collect();
        line.trim_end().to_string()
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.rows).map(|row| self.line(row)).collect()
    }

    pub fn contains(&self, needle: &str) -> bool {
        (0..self.rows).any(|row| self.line(row).contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_survive_the_atomic_form() {
        let modes = Modes {
            win32_input: true,
            alt_screen: true,
            mouse: true,
            ..Modes::default()
        };

        assert_eq!(Modes::from_bits(modes.to_bits()), modes);
    }

    #[test]
    fn a_line_skips_the_right_half_of_wide_glyphs() {
        let mut flags = CellFlags::default();
        flags.insert(CellFlags::WIDE_SPACER);
        let glyph = Cell {
            ch: '語',
            ..Cell::default()
        };
        let spacer = Cell {
            flags,
            ..Cell::default()
        };
        let screen = Screen {
            cols: 3,
            rows: 1,
            cells: vec![glyph, spacer, Cell::default()],
            ..Screen::default()
        };

        assert_eq!(screen.line(0), "語");
    }
}
