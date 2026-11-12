//! Key presses to bytes. ConPTY asks for win32-input-mode (9001) as soon as it starts, and only
//! its records carry modifiers an xterm sequence cannot express, such as Shift+Enter in a console
//! program. Everything else is the plain xterm encoding. A Unix pseudoterminal has no 9001 mode,
//! so there the plain xterm encoding is the whole answer.

#[cfg(windows)]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MAPVK_VK_TO_VSC, MapVirtualKeyW, VkKeyScanW,
};

use crate::screen::Modes;

#[cfg(windows)]
const SHIFT_PRESSED: u32 = 0x10;
#[cfg(windows)]
const LEFT_CTRL_PRESSED: u32 = 0x08;
#[cfg(windows)]
const LEFT_ALT_PRESSED: u32 = 0x02;
#[cfg(windows)]
const ENHANCED_KEY: u32 = 0x100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F12.
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const SHIFT: Self = Self {
        shift: true,
        ctrl: false,
        alt: false,
    };
    pub const CTRL: Self = Self {
        shift: false,
        ctrl: true,
        alt: false,
    };
    pub const ALT: Self = Self {
        shift: false,
        ctrl: false,
        alt: true,
    };

    #[cfg(windows)]
    fn control_state(self) -> u32 {
        let flag = |on: bool, bit: u32| if on { bit } else { 0 };
        flag(self.shift, SHIFT_PRESSED)
            | flag(self.ctrl, LEFT_CTRL_PRESSED)
            | flag(self.alt, LEFT_ALT_PRESSED)
    }

    /// The modifier parameter of xterm sequences, 1 when there are none.
    fn xterm(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self != Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    pub fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }

    pub fn plain(key: Key) -> Self {
        Self::new(key, Mods::default())
    }
}

/// Shift+Enter has no single encoding: a TUI that reads VT sees a record as a bare CR and sends
/// the message, a console program never parses `CSI 13;2u`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftEnter {
    #[default]
    Win32Record,
    CsiU,
}

#[cfg(windows)]
struct Record {
    vk: u16,
    unicode: u16,
    control: u32,
}

#[cfg(windows)]
impl Record {
    fn write(&self, out: &mut Vec<u8>, down: bool) {
        let Record {
            vk,
            unicode,
            control,
        } = *self;
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) };
        let down = down as u8;
        let text = format!("\x1b[{vk};{scan};{unicode};{down};{control};1_");
        out.extend_from_slice(text.as_bytes());
    }
}

#[cfg(windows)]
fn virtual_key(ch: char) -> u16 {
    if ch.is_ascii_alphanumeric() {
        return ch.to_ascii_uppercase() as u16;
    }
    let mut units = [0; 2];
    let scanned = unsafe { VkKeyScanW(ch.encode_utf16(&mut units)[0]) };
    (scanned as u16) & 0xFF
}

fn control_code(ch: char) -> Option<u8> {
    match ch {
        '@'..='_' => Some(ch as u8 & 0x1F),
        'a'..='z' => Some(ch as u8 & 0x1F),
        ' ' => Some(0),
        '?' => Some(0x7F),
        _ => None,
    }
}

#[cfg(windows)]
fn record(event: KeyEvent) -> Record {
    let KeyEvent { key, mods } = event;
    let mut control = mods.control_state();
    let (vk, unicode) = match key {
        Key::Char(ch) => {
            let code = mods.ctrl.then(|| control_code(ch)).flatten();
            let unicode = code.map_or(ch as u32 as u16, u16::from);
            (virtual_key(ch), unicode)
        }
        Key::Enter => (0x0D, b'\r' as u16),
        Key::Tab => (0x09, b'\t' as u16),
        // Consoles expect BS here and DEL with Ctrl, the reverse of what the keyboard writes.
        Key::Backspace => (0x08, if mods.ctrl { 0x7F } else { 0x08 }),
        Key::Escape => (0x1B, 0x1B),
        Key::F(number) => (0x6F + number as u16, 0),
        navigation => {
            control |= ENHANCED_KEY;
            let vk = match navigation {
                Key::PageUp => 0x21,
                Key::PageDown => 0x22,
                Key::End => 0x23,
                Key::Home => 0x24,
                Key::Left => 0x25,
                Key::Up => 0x26,
                Key::Right => 0x27,
                Key::Down => 0x28,
                Key::Insert => 0x2D,
                _ => 0x2E,
            };
            (vk, 0)
        }
    };
    Record {
        vk,
        unicode,
        control,
    }
}

/// Letters, digits, spaces and control codes pass through ConPTY as UTF-8. The system conhost
/// turns any other character the keyboard layout has no key for into Alt and numpad presses, `{`
/// on a Russian layout for one, and programs that ignore key releases lose it. A record of our
/// own carries the character itself, whatever the layout.
#[cfg(windows)]
fn passes_as_text(ch: char) -> bool {
    ch.is_alphanumeric() || ch == ' ' || ch.is_control()
}

/// Text for the program, sent the way no keyboard layout can change.
pub fn encode_text(text: &str, modes: Modes) -> Vec<u8> {
    #[cfg(windows)]
    {
        if !modes.win32_input {
            return text.as_bytes().to_vec();
        }
        let mut out = Vec::with_capacity(text.len());
        let mut units = [0; 2];
        for ch in text.chars() {
            if passes_as_text(ch) {
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
            for &unicode in ch.encode_utf16(&mut units).iter() {
                let record = Record {
                    vk: 0,
                    unicode,
                    control: 0,
                };
                record.write(&mut out, true);
                record.write(&mut out, false);
            }
        }
        out
    }
    #[cfg(unix)]
    {
        let _ = modes;
        text.as_bytes().to_vec()
    }
}

/// `CSI 1 ; m X` with modifiers, otherwise `CSI X`, or `SS3 X` in application mode.
fn cursor_key(out: &mut Vec<u8>, letter: char, mods: Mods, application: bool) {
    let text = match (mods.any(), application) {
        (true, _) => format!("\x1b[1;{}{letter}", mods.xterm()),
        (false, true) => format!("\x1bO{letter}"),
        (false, false) => format!("\x1b[{letter}"),
    };
    out.extend_from_slice(text.as_bytes());
}

fn tilde_key(out: &mut Vec<u8>, code: u8, mods: Mods) {
    let text = if mods.any() {
        format!("\x1b[{code};{}~", mods.xterm())
    } else {
        format!("\x1b[{code}~")
    };
    out.extend_from_slice(text.as_bytes());
}

fn function_key(out: &mut Vec<u8>, number: u8, mods: Mods) {
    match number {
        1..=4 => cursor_key(out, (b'P' + number - 1) as char, mods, true),
        5 => tilde_key(out, 15, mods),
        6..=10 => tilde_key(out, 11 + number, mods),
        _ => tilde_key(out, 12 + number, mods),
    }
}

fn legacy(out: &mut Vec<u8>, event: KeyEvent, modes: Modes) {
    let KeyEvent { key, mods } = event;
    let escape = mods.alt && matches!(key, Key::Char(_) | Key::Enter | Key::Backspace);
    if escape {
        out.push(0x1B);
    }
    let application = modes.app_cursor;
    match key {
        Key::Char(ch) => match mods.ctrl.then(|| control_code(ch)).flatten() {
            Some(code) => out.push(code),
            None => out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
        },
        Key::Enter => out.push(b'\r'),
        Key::Tab if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => out.push(b'\t'),
        Key::Backspace => out.push(if mods.ctrl { 0x08 } else { 0x7F }),
        Key::Escape => out.push(0x1B),
        Key::Up => cursor_key(out, 'A', mods, application),
        Key::Down => cursor_key(out, 'B', mods, application),
        Key::Right => cursor_key(out, 'C', mods, application),
        Key::Left => cursor_key(out, 'D', mods, application),
        Key::Home => cursor_key(out, 'H', mods, application),
        Key::End => cursor_key(out, 'F', mods, application),
        Key::Insert => tilde_key(out, 2, mods),
        Key::Delete => tilde_key(out, 3, mods),
        Key::PageUp => tilde_key(out, 5, mods),
        Key::PageDown => tilde_key(out, 6, mods),
        Key::F(number) => function_key(out, number, mods),
    }
}

pub fn encode(event: KeyEvent, modes: Modes, shift_enter: ShiftEnter) -> Vec<u8> {
    let mut out = Vec::new();
    let KeyEvent { key, mods } = event;
    if key == Key::Enter && mods == Mods::SHIFT && shift_enter == ShiftEnter::CsiU {
        return b"\x1b[13;2u".to_vec();
    }
    if let Key::Char(ch) = key
        && !mods.ctrl
        && !mods.alt
    {
        return encode_text(ch.encode_utf8(&mut [0; 4]), modes);
    }
    if !modes.win32_input {
        legacy(&mut out, event, modes);
        return out;
    }
    #[cfg(windows)]
    {
        let record = record(event);
        record.write(&mut out, true);
        record.write(&mut out, false);
        out
    }
    #[cfg(unix)]
    {
        legacy(&mut out, event, modes);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN32: Modes = Modes {
        win32_input: true,
        bracketed_paste: false,
        app_cursor: false,
        alt_screen: false,
        focus_events: false,
        mouse: false,
    };

    fn legacy(key: Key, mods: Mods) -> String {
        let bytes = encode(
            KeyEvent::new(key, mods),
            Modes::default(),
            ShiftEnter::Win32Record,
        );
        String::from_utf8(bytes).unwrap()
    }

    #[cfg(windows)]
    fn record(key: Key, mods: Mods) -> String {
        let bytes = encode(KeyEvent::new(key, mods), WIN32, ShiftEnter::Win32Record);
        String::from_utf8(bytes).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn punctuation_goes_as_a_record_and_letters_as_text() {
        let text = String::from_utf8(encode_text("a{ж\r", WIN32)).unwrap();
        assert_eq!(text, "a\x1b[0;0;123;1;0;1_\x1b[0;0;123;0;0;1_ж\r");
        assert_eq!(encode_text("a{", Modes::default()), b"a{");
    }

    #[test]
    fn backspace_is_del_and_ctrl_backspace_is_bs() {
        assert_eq!(legacy(Key::Backspace, Mods::default()), "\x7f");
        assert_eq!(legacy(Key::Backspace, Mods::CTRL), "\x08");
    }

    #[cfg(windows)]
    #[test]
    fn a_record_says_press_and_release() {
        let enter = record(Key::Enter, Mods::SHIFT);

        assert_eq!(enter, "\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_");
    }

    #[cfg(windows)]
    #[test]
    fn backspace_records_are_the_reverse_of_the_keyboard() {
        assert!(record(Key::Backspace, Mods::default()).starts_with("\x1b[8;14;8;1;0;1_"));
        assert!(record(Key::Backspace, Mods::CTRL).starts_with("\x1b[8;14;127;1;8;1_"));
    }

    #[test]
    fn shift_enter_follows_the_program() {
        let event = KeyEvent::new(Key::Enter, Mods::SHIFT);

        assert_eq!(encode(event, WIN32, ShiftEnter::CsiU), b"\x1b[13;2u");
        assert_ne!(encode(event, WIN32, ShiftEnter::Win32Record), b"\x1b[13;2u");
    }

    #[cfg(windows)]
    #[test]
    fn letters_stay_plain_and_symbols_carry_their_utf16() {
        assert_eq!(record(Key::Char('я'), Mods::SHIFT), "я");
        let emoji = record(Key::Char('😀'), Mods::default());
        assert!(emoji.starts_with("\x1b[0;0;55357;1;0;1_\x1b[0;0;55357;0;0;1_"));
        assert!(emoji.ends_with("\x1b[0;0;56832;0;0;1_"));
    }

    #[cfg(windows)]
    #[test]
    fn ctrl_c_is_a_record_with_the_control_code() {
        assert!(record(Key::Char('c'), Mods::CTRL).starts_with("\x1b[67;46;3;1;8;1_"));
    }

    #[test]
    fn xterm_sequences_carry_modifiers() {
        assert_eq!(legacy(Key::Up, Mods::default()), "\x1b[A");
        assert_eq!(legacy(Key::Left, Mods::ALT), "\x1b[1;3D");
        assert_eq!(legacy(Key::Delete, Mods::CTRL), "\x1b[3;5~");
        assert_eq!(legacy(Key::F(1), Mods::default()), "\x1bOP");
        assert_eq!(legacy(Key::F(5), Mods::default()), "\x1b[15~");
        assert_eq!(legacy(Key::F(10), Mods::default()), "\x1b[21~");
        assert_eq!(legacy(Key::F(12), Mods::default()), "\x1b[24~");
        assert_eq!(legacy(Key::Tab, Mods::SHIFT), "\x1b[Z");
        assert_eq!(legacy(Key::Char('c'), Mods::CTRL), "\x03");
        assert_eq!(legacy(Key::Char('x'), Mods::ALT), "\x1bx");
    }

    #[test]
    fn application_mode_changes_plain_arrows_only() {
        let modes = Modes {
            app_cursor: true,
            ..Modes::default()
        };
        let plain = encode(KeyEvent::plain(Key::Up), modes, ShiftEnter::Win32Record);
        let shifted = encode(
            KeyEvent::new(Key::Up, Mods::SHIFT),
            modes,
            ShiftEnter::Win32Record,
        );

        assert_eq!(plain, b"\x1bOA");
        assert_eq!(shifted, b"\x1b[1;2A");
    }
}
