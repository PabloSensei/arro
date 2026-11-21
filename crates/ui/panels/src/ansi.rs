//! Console output into lines of styled text. Knows SGR colors and bold, drops every other
//! escape sequence, and a lone `\r` starts the line over, as a progress bar expects. The
//! colours of the console and the painting of a line are here too, so the consoles of Run
//! and Debug show a line the same way.

use std::collections::BTreeMap;
use std::ops::Range;

use ui_kit::{FontWeight, HighlightStyle, Hsla, StyledText, Theme, rgb};

/// The 16 terminal colors, dark theme first.
const ANSI_DARK: [u32; 16] = [
    0x5a5d63, 0xf75464, 0x5fb865, 0xe0bb65, 0x548af7, 0xc77dbb, 0x2aacb8, 0xbcbec4, 0x868a91,
    0xff7a85, 0x89d18e, 0xf2c55c, 0x7aa7ff, 0xe0a0d6, 0x58c9d4, 0xffffff,
];
const ANSI_LIGHT: [u32; 16] = [
    0x000000, 0xc7222d, 0x067d17, 0x9e880d, 0x0033b3, 0x871094, 0x0f7b8a, 0x6f737a, 0x818594,
    0xe55765, 0x2e9c3b, 0xb59b1b, 0x3574f0, 0xb350bf, 0x1996a6, 0x1e1f22,
];

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// One of the 16 terminal colors.
    pub color: Option<u8>,
    pub bold: bool,
    /// Came from stderr.
    pub error: bool,
    /// The user typed it into the console, not the program.
    pub input: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

impl Line {
    fn push(&mut self, ch: char, style: Style) {
        let start = self.text.len();
        self.text.push(ch);
        let end = self.text.len();
        match self.spans.last_mut() {
            Some((range, last)) if *last == style && range.end == start => range.end = end,
            _ => self.spans.push((start..end, style)),
        }
    }

    pub fn plain(text: impl Into<String>, style: Style) -> Self {
        let text = text.into();
        let spans = vec![(0..text.len(), style)];
        Self { text, spans }
    }
}

#[derive(Default)]
enum Escape {
    #[default]
    None,
    /// Saw ESC.
    Start,
    /// Inside `ESC [`, collecting parameters.
    Csi(String),
}

#[derive(Default)]
pub struct AnsiParser {
    style: Style,
    current: Line,
    escape: Escape,
    carriage: bool,
}

impl AnsiParser {
    /// Feeds a piece of output. Lines it completes go to `done`.
    pub fn push(&mut self, text: &str, error: bool, done: &mut Vec<Line>) {
        for ch in text.chars() {
            match std::mem::take(&mut self.escape) {
                Escape::Start if ch == '[' => self.escape = Escape::Csi(String::new()),
                Escape::Start => {}
                Escape::Csi(mut params) => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        if ch == 'm' {
                            self.apply(&params);
                        }
                    } else {
                        params.push(ch);
                        self.escape = Escape::Csi(params);
                    }
                }
                Escape::None => self.char(ch, error, done),
            }
        }
    }

    /// The line that has not ended yet.
    pub fn current(&self) -> &Line {
        &self.current
    }

    /// The program is gone: what it left unfinished is a line too.
    pub fn finish(&mut self, done: &mut Vec<Line>) {
        if !self.current.text.is_empty() {
            done.push(std::mem::take(&mut self.current));
        }
        *self = Self::default();
    }

    fn char(&mut self, ch: char, error: bool, done: &mut Vec<Line>) {
        let carriage = std::mem::take(&mut self.carriage);
        match ch {
            '\u{1b}' => self.escape = Escape::Start,
            '\n' => done.push(std::mem::take(&mut self.current)),
            '\r' => self.carriage = true,
            '\t' => self.put(ch, error, carriage),
            ch if ch.is_control() => {}
            ch => self.put(ch, error, carriage),
        }
    }

    fn put(&mut self, ch: char, error: bool, carriage: bool) {
        if carriage {
            self.current = Line::default();
        }
        let style = Style {
            error,
            ..self.style
        };
        self.current.push(ch, style);
    }

    fn apply(&mut self, params: &str) {
        let mut codes = params
            .split(';')
            .map(|code| code.parse::<u16>().unwrap_or(0));
        while let Some(code) = codes.next() {
            match code {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                22 => self.style.bold = false,
                30..=37 => self.style.color = Some((code - 30) as u8),
                90..=97 => self.style.color = Some((code - 90 + 8) as u8),
                39 => self.style.color = None,
                38 | 48 => match codes.next() {
                    Some(5) => {
                        let index = codes.next();
                        if code == 38 {
                            self.style.color = index.filter(|index| *index < 16).map(|i| i as u8);
                        }
                    }
                    Some(2) => {
                        codes.nth(2);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// The unfinished line of every process and stream that writes into one console. Each
/// writer has a parser of its own, so the end of a line of one process never joins a
/// piece of the line of another, and stdout and stderr are separate as well.
#[derive(Default)]
pub struct OutputBuffers {
    parsers: BTreeMap<(u32, bool), AnsiParser>,
}

impl OutputBuffers {
    /// Feeds a piece of the output of `process`; `error` says that it came from stderr.
    /// Lines it completes go to `done`.
    pub fn push(&mut self, process: u32, error: bool, text: &str, done: &mut Vec<Line>) {
        self.parsers
            .entry((process, error))
            .or_default()
            .push(text, error, done);
    }

    /// The process is gone: what it left unfinished is a line too.
    pub fn finish(&mut self, process: u32, done: &mut Vec<Line>) {
        for error in [false, true] {
            if let Some(mut parser) = self.parsers.remove(&(process, error)) {
                parser.finish(done);
            }
        }
    }

    /// No process writes any more: what every one of them left unfinished is a line too.
    pub fn finish_all(&mut self, done: &mut Vec<Line>) {
        for (_, mut parser) in std::mem::take(&mut self.parsers) {
            parser.finish(done);
        }
    }

    /// The lines that have not ended yet, one per writer.
    pub fn unfinished(&self) -> impl Iterator<Item = &Line> {
        self.parsers
            .values()
            .map(AnsiParser::current)
            .filter(|line| !line.text.is_empty())
    }

    /// The first line that has not ended yet, for a console that shows one.
    pub fn current(&self) -> Option<&Line> {
        self.unfinished().next()
    }
}

/// One line of a console that keeps its text with the codes: the text without the codes
/// and the styles the codes asked for. A console that stores the raw line for its search
/// and its links shows it through this.
pub fn parse_line(text: &str, error: bool) -> Line {
    let mut parser = AnsiParser::default();
    let mut done = Vec::new();
    parser.push(text, error, &mut done);
    if !parser.current().text.is_empty() {
        done.push(parser.current().clone());
    }
    done.pop().unwrap_or_default()
}

/// The text of a console line without the escape sequences, for a search, a filter or a
/// link that reads the line the reader sees.
pub fn plain(text: &str) -> String {
    parse_line(text, false).text
}

/// The text of a line in the colours of the console, with no layout of its own. The
/// places a search found are painted over those colours.
pub fn highlighted(line: &Line, theme: &Theme, matches: &[Range<usize>]) -> StyledText {
    let mut edges = vec![0, line.text.len()];
    for (range, _) in &line.spans {
        edges.push(range.start);
        edges.push(range.end);
    }
    for range in matches {
        edges.push(range.start.min(line.text.len()));
        edges.push(range.end.min(line.text.len()));
    }
    edges.sort_unstable();
    edges.dedup();
    let mut highlights = Vec::new();
    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end || !line.text.is_char_boundary(start) || !line.text.is_char_boundary(end) {
            continue;
        }
        let found = matches
            .iter()
            .any(|range| range.start <= start && range.end >= end);
        let highlight = if found {
            HighlightStyle {
                color: Some(theme.match_text),
                background_color: Some(theme.match_bg),
                ..HighlightStyle::default()
            }
        } else {
            line.spans
                .iter()
                .find(|(range, _)| range.start <= start && range.end >= end)
                .map_or_else(HighlightStyle::default, |(_, style)| {
                    console_highlight(style, theme)
                })
        };
        highlights.push((start..end, highlight));
    }
    StyledText::new(line.text.clone()).with_highlights(highlights)
}

/// The style of one span of the console, in the colours of the theme.
fn console_highlight(style: &Style, theme: &Theme) -> HighlightStyle {
    let palette = if theme.dark { &ANSI_DARK } else { &ANSI_LIGHT };
    let color = match (style.input, style.color, style.error) {
        (true, _, _) => Some(theme.console_input),
        (false, Some(index), _) => Some(color(palette[usize::from(index)])),
        (false, None, true) => Some(theme.error),
        (false, None, false) => None,
    };
    HighlightStyle {
        color,
        font_weight: style.bold.then_some(FontWeight::BOLD),
        ..HighlightStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{AnsiParser, Line, Style};

    fn lines(text: &str) -> (Vec<Line>, Line) {
        let mut parser = AnsiParser::default();
        let mut done = Vec::new();
        parser.push(text, false, &mut done);
        (done, parser.current().clone())
    }

    #[test]
    fn plain_text_splits_into_lines() {
        let (done, current) = lines("one\ntwo\nthr");
        let texts: Vec<_> = done.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert_eq!(current.text, "thr");
    }

    #[test]
    fn colors_and_bold_become_spans() {
        let (done, _) = lines("a\u{1b}[1;31mbc\u{1b}[0md\n");
        let line = &done[0];
        assert_eq!(line.text, "abcd");
        let red = Style {
            color: Some(1),
            bold: true,
            error: false,
            input: false,
        };
        assert_eq!(
            line.spans,
            [
                (0..1, Style::default()),
                (1..3, red),
                (3..4, Style::default())
            ]
        );
    }

    #[test]
    fn bright_and_indexed_colors_are_known() {
        let (_, current) = lines("\u{1b}[92mx\u{1b}[38;5;4my\u{1b}[38;2;1;2;3mz");
        let colors: Vec<_> = current.spans.iter().map(|(_, style)| style.color).collect();
        // A true color is not one of the 16, so the last color stays.
        assert_eq!(current.text, "xyz");
        assert_eq!(colors, [Some(10), Some(4)]);
    }

    #[test]
    fn other_sequences_are_dropped() {
        let (_, current) = lines("\u{1b}[2Ka\u{1b}[?25lb\u{1b}7c");
        assert_eq!(current.text, "abc");
    }

    #[test]
    fn a_lone_carriage_return_starts_the_line_over() {
        let (done, current) = lines("10%\r20%\r\nnext\r");
        assert_eq!(done[0].text, "20%");
        assert_eq!(current.text, "next");
    }

    #[test]
    fn a_sequence_split_between_pieces_still_applies() {
        let mut parser = AnsiParser::default();
        let mut done = Vec::new();
        parser.push("\u{1b}[3", false, &mut done);
        parser.push("4mx", true, &mut done);
        let style = parser.current().spans[0].1;
        assert_eq!((style.color, style.error), (Some(4), true));
    }
}
