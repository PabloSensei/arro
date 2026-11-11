//! Bytes on disk and back. `encode(decode(bytes))` gives the same bytes, line ends and
//! BOM included; when that cannot hold, the file is opened read-only.

use std::borrow::Cow;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

/// Line ends of a file. A mixed file keeps one entry per `\n` of the text, because
/// nothing may rewrite the lines a user did not touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EolStyle {
    Uniform(Eol),
    Mixed { lines: Vec<Eol>, default: Eol },
}

/// The line end table of the lines that an edit replaced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EolPatch {
    line: usize,
    removed: Vec<Eol>,
    inserted: usize,
}

impl EolStyle {
    /// New lines take the most common line end of the file.
    pub fn default_eol(&self) -> Eol {
        match self {
            Self::Uniform(eol) => *eol,
            Self::Mixed { default, .. } => *default,
        }
    }

    /// `removed` line ends starting with the `line`-th were replaced by `inserted` new ones.
    pub(crate) fn splice(&mut self, line: usize, removed: usize, inserted: usize) -> EolPatch {
        let Self::Mixed { lines, default } = self else {
            return EolPatch::default();
        };
        let fresh = std::iter::repeat_n(*default, inserted);
        let removed: Vec<Eol> = lines.splice(line..line + removed, fresh).collect();
        EolPatch {
            line,
            removed,
            inserted,
        }
    }

    pub(crate) fn restore(&mut self, patch: &EolPatch) {
        if let Self::Mixed { lines, .. } = self {
            lines.splice(
                patch.line..patch.line + patch.inserted,
                patch.removed.iter().copied(),
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Legacy(&'static Encoding),
}

impl TextEncoding {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Legacy(encoding) => encoding.name(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFormat {
    pub encoding: TextEncoding,
    pub bom: bool,
    pub eol: EolStyle,
}

impl FileFormat {
    /// The same file with one line end everywhere.
    pub fn with_eol(&self, eol: Eol) -> Self {
        Self {
            eol: EolStyle::Uniform(eol),
            ..self.clone()
        }
    }

    /// The format for a name from the status bar: `UTF-8`, `UTF-8 BOM`, `UTF-16LE`,
    /// `UTF-16BE` or the label of a legacy encoding. `None` for a name nobody knows.
    pub fn with_encoding_name(&self, name: &str) -> Option<Self> {
        let (encoding, bom) = match name {
            "UTF-8" => (TextEncoding::Utf8, false),
            "UTF-8 BOM" => (TextEncoding::Utf8, true),
            "UTF-16LE" => (TextEncoding::Utf16Le, true),
            "UTF-16BE" => (TextEncoding::Utf16Be, true),
            label => (
                TextEncoding::Legacy(Encoding::for_label(label.as_bytes())?),
                false,
            ),
        };
        Some(Self {
            encoding,
            bom,
            ..self.clone()
        })
    }
}

impl Default for FileFormat {
    fn default() -> Self {
        Self {
            encoding: TextEncoding::Utf8,
            bom: false,
            eol: EolStyle::Uniform(Eol::Lf),
        }
    }
}

pub struct Decoded {
    /// Text with `\n` line ends only.
    pub text: String,
    pub format: FileFormat,
    /// False when saving could not restore the bytes: open such a file read-only.
    pub lossless: bool,
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];

pub fn decode(bytes: &[u8]) -> Decoded {
    let (encoding, bom, body) = sniff(bytes);
    let (text, clean): (Cow<'_, str>, bool) = match encoding {
        TextEncoding::Utf8 => match std::str::from_utf8(body) {
            Ok(text) => (Cow::Borrowed(text), true),
            Err(_) => (String::from_utf8_lossy(body), false),
        },
        TextEncoding::Utf16Le => decoded(encoding_rs::UTF_16LE, body),
        TextEncoding::Utf16Be => decoded(encoding_rs::UTF_16BE, body),
        TextEncoding::Legacy(encoding) => decoded(encoding, body),
    };
    let (text, eol) = split_eols(&text);
    let format = FileFormat { encoding, bom, eol };
    let binary =
        !matches!(encoding, TextEncoding::Utf16Le | TextEncoding::Utf16Be) && body.contains(&0);
    let lossless = clean && !binary && encode(&text, &format) == bytes;
    Decoded {
        text,
        format,
        lossless,
    }
}

fn decoded<'a>(encoding: &'static Encoding, body: &'a [u8]) -> (Cow<'a, str>, bool) {
    let (text, had_errors) = encoding.decode_without_bom_handling(body);
    (text, !had_errors)
}

fn sniff(bytes: &[u8]) -> (TextEncoding, bool, &[u8]) {
    if let Some(body) = bytes.strip_prefix(UTF8_BOM) {
        return (TextEncoding::Utf8, true, body);
    }
    if let Some(body) = bytes.strip_prefix(UTF16LE_BOM) {
        return (TextEncoding::Utf16Le, true, body);
    }
    if let Some(body) = bytes.strip_prefix(UTF16BE_BOM) {
        return (TextEncoding::Utf16Be, true, body);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return (TextEncoding::Utf8, false, bytes);
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, Utf8Detection::Deny);
    (TextEncoding::Legacy(encoding), false, bytes)
}

/// Turns `\r\n` and `\r` into `\n`, for text that comes from outside the buffer.
pub fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

fn split_eols(text: &str) -> (String, EolStyle) {
    let mut lines = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['\n', '\r']) {
        out.push_str(&rest[..at]);
        out.push('\n');
        let (eol, width) = if rest[at..].starts_with("\r\n") {
            (Eol::CrLf, 2)
        } else if rest[at..].starts_with('\r') {
            (Eol::Cr, 1)
        } else {
            (Eol::Lf, 1)
        };
        lines.push(eol);
        rest = &rest[at + width..];
    }
    out.push_str(rest);
    (out, eol_style(lines))
}

fn eol_style(lines: Vec<Eol>) -> EolStyle {
    let count = |eol: Eol| lines.iter().filter(|line| **line == eol).count();
    let counts = [
        (count(Eol::Lf), Eol::Lf),
        (count(Eol::CrLf), Eol::CrLf),
        (count(Eol::Cr), Eol::Cr),
    ];
    let (most, default) = counts
        .into_iter()
        .max_by_key(|(count, _)| *count)
        .unwrap_or((0, Eol::Lf));
    if most == lines.len() {
        EolStyle::Uniform(if most == 0 { Eol::Lf } else { default })
    } else {
        EolStyle::Mixed { lines, default }
    }
}

/// The bytes of `text`, a text with `\n` line ends, in `format`.
pub fn encode(text: &str, format: &FileFormat) -> Vec<u8> {
    let text = join_eols(text, &format.eol);
    let mut bytes = Vec::with_capacity(text.len() + 3);
    match format.encoding {
        TextEncoding::Utf8 => {
            if format.bom {
                bytes.extend_from_slice(UTF8_BOM);
            }
            bytes.extend_from_slice(text.as_bytes());
        }
        TextEncoding::Utf16Le => {
            if format.bom {
                bytes.extend_from_slice(UTF16LE_BOM);
            }
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        }
        TextEncoding::Utf16Be => {
            if format.bom {
                bytes.extend_from_slice(UTF16BE_BOM);
            }
            bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        }
        TextEncoding::Legacy(encoding) => {
            let (encoded, _, _) = encoding.encode(&text);
            bytes.extend_from_slice(&encoded);
        }
    }
    bytes
}

fn join_eols<'a>(text: &'a str, style: &EolStyle) -> Cow<'a, str> {
    match style {
        EolStyle::Uniform(Eol::Lf) => Cow::Borrowed(text),
        EolStyle::Uniform(eol) => Cow::Owned(text.replace('\n', eol.as_str())),
        EolStyle::Mixed { lines, default } => {
            let mut out = String::with_capacity(text.len() + lines.len());
            let mut pieces = text.split('\n').peekable();
            let mut eols = lines.iter();
            while let Some(piece) = pieces.next() {
                out.push_str(piece);
                if pieces.peek().is_some() {
                    out.push_str(eols.next().unwrap_or(default).as_str());
                }
            }
            Cow::Owned(out)
        }
    }
}
