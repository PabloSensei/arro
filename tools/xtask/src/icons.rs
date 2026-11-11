//! Generates the Arro icon set from the Lucide static sources.
//!
//! Every file keeps the name the application already uses. The table below fixes
//! the shape and the colour role of each icon; the sizes match the files that were
//! in the repository before the switch. Running the command again writes the same
//! bytes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

const ICON_DIR: &str = "crates/ui/ui-kit/assets/icons";

/// The Lucide release the checked-in icons are based on.
pub(crate) const LUCIDE_VERSION: &str = "1.70.0";

/// Colours of one theme. `main`, `accent` and `over_accent` are the
/// `text_muted`, `accent` and `over_accent` tokens of `crates/editor/theme`;
/// the statuses follow the theme warning and the existing notification palette.
/// Symbol and file colours are the darker ends of the theme's code hues, so a
/// white letter keeps its contrast at 16 px.
struct Palette {
    main: &'static str,
    accent: &'static str,
    over_accent: &'static str,
    error: &'static str,
    warning: &'static str,
    success: &'static str,
    info: &'static str,
    class: &'static str,
    interface: &'static str,
    enumeration: &'static str,
    record: &'static str,
    method: &'static str,
    field: &'static str,
    constant: &'static str,
    package: &'static str,
    module: &'static str,
    library: &'static str,
    folder: &'static str,
    source_root: &'static str,
    file_ignored: &'static str,
    file_module: &'static str,
    file_source: &'static str,
    file_text: &'static str,
    file_xml: &'static str,
}

const LIGHT: Palette = Palette {
    main: "#5e5a54",
    accent: "#c2410c",
    over_accent: "#ffffff",
    error: "#d6455a",
    warning: "#b5790a",
    success: "#2f8f4e",
    info: "#2f6fbf",
    class: "#1d5fa8",
    interface: "#0b7363",
    enumeration: "#5b3fc4",
    record: "#564f8c",
    method: "#2f7a52",
    field: "#356a19",
    constant: "#b02a4f",
    package: "#8a5a00",
    module: "#c2410c",
    library: "#6f6a63",
    folder: "#b5790a",
    source_root: "#c2410c",
    file_ignored: "#c0453a",
    file_module: "#c2410c",
    file_source: "#b5651d",
    file_text: "#6f6a63",
    file_xml: "#5b3fc4",
};

const DARK: Palette = Palette {
    main: "#a8a39a",
    accent: "#f97316",
    over_accent: "#1c1b1a",
    error: "#e55765",
    warning: "#d9a441",
    success: "#55a76a",
    info: "#4682fa",
    class: "#3b7cc8",
    interface: "#12917a",
    enumeration: "#7a5bd0",
    record: "#6d7896",
    method: "#3e8f63",
    field: "#4e8a2b",
    constant: "#c25670",
    package: "#b5791f",
    module: "#f97316",
    library: "#8a847a",
    folder: "#c9973e",
    source_root: "#f97316",
    file_ignored: "#d95a4e",
    file_module: "#f97316",
    file_source: "#cf7930",
    file_text: "#8a847a",
    file_xml: "#7a5bd0",
};

#[derive(Clone, Copy)]
enum Status {
    Error,
    Warning,
    Success,
    Info,
}

#[derive(Clone, Copy)]
enum Kind {
    Class,
    Interface,
    Enum,
    Record,
    Method,
    Field,
    Constant,
    Package,
    Module,
    Library,
    Folder,
    SourceRoot,
}

#[derive(Clone, Copy)]
enum FileKind {
    Ignored,
    Module,
    Source,
    Text,
    Xml,
}

#[derive(Clone, Copy)]
enum Shape {
    /// A Lucide contour, optionally filled with a tint of the same colour.
    Lucide { name: &'static str, tint: bool },
    /// A rounded square with a letter drawn by strokes, no font is involved.
    Badge(char),
    /// A filled status circle or triangle with a white sign.
    Status(Status),
}

#[derive(Clone, Copy)]
enum Paint {
    Main,
    Accent,
    OverAccent,
    Status(Status),
    Kind(Kind),
    File(FileKind),
}

struct Icon {
    /// The path under `assets/icons` without the `.svg` suffix and the theme suffix.
    name: &'static str,
    size: u32,
    shape: Shape,
    paint: Paint,
}

/// The whole set. Every icon has a light and a dark file, except the ignored-file
/// icon: the widget picks it by name without a suffix, so one file serves both themes.
const ICONS: &[Icon] = &[
    Icon {
        name: "codeInsight/intentionBulb",
        size: 16,
        shape: Shape::Lucide {
            name: "lightbulb",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "fileTypes/gitignore",
        size: 16,
        shape: Shape::Badge('G'),
        paint: Paint::File(FileKind::Ignored),
    },
    Icon {
        name: "fileTypes/ideaModule",
        size: 16,
        shape: Shape::Badge('M'),
        paint: Paint::File(FileKind::Module),
    },
    Icon {
        name: "fileTypes/java",
        size: 16,
        shape: Shape::Badge('J'),
        paint: Paint::File(FileKind::Source),
    },
    Icon {
        name: "fileTypes/text",
        size: 16,
        shape: Shape::Badge('T'),
        paint: Paint::File(FileKind::Text),
    },
    Icon {
        name: "fileTypes/xml",
        size: 16,
        shape: Shape::Badge('X'),
        paint: Paint::File(FileKind::Xml),
    },
    Icon {
        name: "general/add",
        size: 16,
        shape: Shape::Lucide {
            name: "plus",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/arrowDown",
        size: 16,
        shape: Shape::Lucide {
            name: "arrow-down",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/arrowUp",
        size: 16,
        shape: Shape::Lucide {
            name: "arrow-up",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/caseSensitive",
        size: 16,
        shape: Shape::Lucide {
            name: "case-sensitive",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/check",
        size: 16,
        shape: Shape::Lucide {
            name: "check",
            tint: false,
        },
        paint: Paint::OverAccent,
    },
    Icon {
        name: "general/chevronDown",
        size: 16,
        shape: Shape::Lucide {
            name: "chevron-down",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/chevronRight",
        size: 16,
        shape: Shape::Lucide {
            name: "chevron-right",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/chevronUp",
        size: 16,
        shape: Shape::Lucide {
            name: "chevron-up",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/close",
        size: 16,
        shape: Shape::Lucide {
            name: "x",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/closeSmall",
        size: 16,
        shape: Shape::Lucide {
            name: "x",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/collapseAll",
        size: 16,
        shape: Shape::Lucide {
            name: "chevrons-up",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/copy",
        size: 16,
        shape: Shape::Lucide {
            name: "copy",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/expandAll",
        size: 16,
        shape: Shape::Lucide {
            name: "chevrons-down",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/filter",
        size: 16,
        shape: Shape::Lucide {
            name: "filter",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/hide",
        size: 16,
        shape: Shape::Lucide {
            name: "eye-off",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/locate",
        size: 16,
        shape: Shape::Lucide {
            name: "locate",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/menu",
        size: 16,
        shape: Shape::Lucide {
            name: "menu",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/minus",
        size: 16,
        shape: Shape::Lucide {
            name: "minus",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/moreHorizontal",
        size: 16,
        shape: Shape::Lucide {
            name: "ellipsis",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/moreVertical",
        size: 16,
        shape: Shape::Lucide {
            name: "ellipsis-vertical",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/openInToolWindow",
        size: 16,
        shape: Shape::Lucide {
            name: "external-link",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/paste",
        size: 16,
        shape: Shape::Lucide {
            name: "clipboard-paste",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/refresh",
        size: 16,
        shape: Shape::Lucide {
            name: "refresh-cw",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/regex",
        size: 16,
        shape: Shape::Lucide {
            name: "regex",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/search",
        size: 16,
        shape: Shape::Lucide {
            name: "search",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/settings",
        size: 16,
        shape: Shape::Lucide {
            name: "settings",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/show",
        size: 16,
        shape: Shape::Lucide {
            name: "eye",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/user",
        size: 16,
        shape: Shape::Lucide {
            name: "user",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/vcs",
        size: 16,
        shape: Shape::Lucide {
            name: "git-branch",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "general/wholeWord",
        size: 16,
        shape: Shape::Lucide {
            name: "whole-word",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "nodes/class",
        size: 16,
        shape: Shape::Badge('C'),
        paint: Paint::Kind(Kind::Class),
    },
    Icon {
        name: "nodes/constant",
        size: 16,
        shape: Shape::Badge('K'),
        paint: Paint::Kind(Kind::Constant),
    },
    Icon {
        name: "nodes/enum",
        size: 16,
        shape: Shape::Badge('E'),
        paint: Paint::Kind(Kind::Enum),
    },
    Icon {
        name: "nodes/field",
        size: 16,
        shape: Shape::Badge('F'),
        paint: Paint::Kind(Kind::Field),
    },
    Icon {
        name: "nodes/folder",
        size: 16,
        shape: Shape::Lucide {
            name: "folder",
            tint: true,
        },
        paint: Paint::Kind(Kind::Folder),
    },
    Icon {
        name: "nodes/interface",
        size: 16,
        shape: Shape::Badge('I'),
        paint: Paint::Kind(Kind::Interface),
    },
    Icon {
        name: "nodes/library",
        size: 16,
        shape: Shape::Lucide {
            name: "library",
            tint: false,
        },
        paint: Paint::Kind(Kind::Library),
    },
    Icon {
        name: "nodes/method",
        size: 16,
        shape: Shape::Badge('M'),
        paint: Paint::Kind(Kind::Method),
    },
    Icon {
        name: "nodes/module",
        size: 16,
        shape: Shape::Lucide {
            name: "box",
            tint: true,
        },
        paint: Paint::Kind(Kind::Module),
    },
    Icon {
        name: "nodes/package",
        size: 16,
        shape: Shape::Lucide {
            name: "package",
            tint: true,
        },
        paint: Paint::Kind(Kind::Package),
    },
    Icon {
        name: "nodes/record",
        size: 16,
        shape: Shape::Badge('R'),
        paint: Paint::Kind(Kind::Record),
    },
    Icon {
        name: "nodes/sourceRoot",
        size: 16,
        shape: Shape::Lucide {
            name: "folder-root",
            tint: true,
        },
        paint: Paint::Kind(Kind::SourceRoot),
    },
    Icon {
        name: "own/aiSessions",
        size: 20,
        shape: Shape::Lucide {
            name: "sparkles",
            tint: false,
        },
        paint: Paint::Accent,
    },
    Icon {
        name: "run/debug",
        size: 16,
        shape: Shape::Lucide {
            name: "bug",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "run/restart",
        size: 16,
        shape: Shape::Lucide {
            name: "rotate-ccw",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "run/run",
        size: 16,
        shape: Shape::Lucide {
            name: "play",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "run/stop",
        size: 16,
        shape: Shape::Lucide {
            name: "square",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "status/error",
        size: 16,
        shape: Shape::Status(Status::Error),
        paint: Paint::Status(Status::Error),
    },
    Icon {
        name: "status/info",
        size: 16,
        shape: Shape::Status(Status::Info),
        paint: Paint::Status(Status::Info),
    },
    Icon {
        name: "status/success",
        size: 16,
        shape: Shape::Status(Status::Success),
        paint: Paint::Status(Status::Success),
    },
    Icon {
        name: "status/warning",
        size: 16,
        shape: Shape::Status(Status::Warning),
        paint: Paint::Status(Status::Warning),
    },
    Icon {
        name: "toolwindows/bookmarks",
        size: 16,
        shape: Shape::Lucide {
            name: "bookmark",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/build",
        size: 16,
        shape: Shape::Lucide {
            name: "hammer",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/commit",
        size: 16,
        shape: Shape::Lucide {
            name: "git-commit-horizontal",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/debug",
        size: 16,
        shape: Shape::Lucide {
            name: "bug",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/notifications",
        size: 16,
        shape: Shape::Lucide {
            name: "bell",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/problems",
        size: 16,
        shape: Shape::Lucide {
            name: "circle-alert",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/project",
        size: 16,
        shape: Shape::Lucide {
            name: "folder-tree",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/run",
        size: 16,
        shape: Shape::Lucide {
            name: "play",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/services",
        size: 16,
        shape: Shape::Lucide {
            name: "server",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/structure",
        size: 16,
        shape: Shape::Lucide {
            name: "list-tree",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/tasks",
        size: 16,
        shape: Shape::Lucide {
            name: "hammer",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/terminal",
        size: 16,
        shape: Shape::Lucide {
            name: "square-terminal",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "toolwindows/vcs",
        size: 16,
        shape: Shape::Lucide {
            name: "git-branch",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/close",
        size: 16,
        shape: Shape::Lucide {
            name: "x",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/closeSmall",
        size: 16,
        shape: Shape::Lucide {
            name: "x",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/maximize",
        size: 16,
        shape: Shape::Lucide {
            name: "square",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/maximizeSmall",
        size: 16,
        shape: Shape::Lucide {
            name: "square",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/minimize",
        size: 16,
        shape: Shape::Lucide {
            name: "minus",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/minimizeSmall",
        size: 16,
        shape: Shape::Lucide {
            name: "minus",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/restore",
        size: 16,
        shape: Shape::Lucide {
            name: "copy",
            tint: false,
        },
        paint: Paint::Main,
    },
    Icon {
        name: "windows/restoreSmall",
        size: 16,
        shape: Shape::Lucide {
            name: "copy",
            tint: false,
        },
        paint: Paint::Main,
    },
];

pub fn run(root: &Path, lucide: &Path) -> Result {
    let written = generate(root, lucide)?;
    println!(
        "icons: wrote {written} files from Lucide {LUCIDE_VERSION} in {}",
        lucide.display()
    );
    Ok(())
}

/// Renders and writes the whole set. Returns the number of files.
fn generate(root: &Path, lucide: &Path) -> Result<usize> {
    let source_dir = lucide_source_dir(lucide);
    let target_dir = root.join(ICON_DIR);
    let mut written = 0;
    for icon in ICONS {
        for variant in variants(icon) {
            let text = render_icon(icon, variant.palette, &source_dir)?;
            let path = target_dir.join(format!("{}{}.svg", icon.name, variant.suffix));
            if fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
                fs::write(&path, text.as_bytes())?;
            }
            written += 1;
        }
    }
    Ok(written)
}

fn lucide_source_dir(lucide: &Path) -> PathBuf {
    if lucide.join("icons").is_dir() {
        lucide.join("icons")
    } else {
        lucide.to_path_buf()
    }
}

struct Variant {
    suffix: &'static str,
    palette: &'static Palette,
}

fn variants(icon: &Icon) -> Vec<Variant> {
    if icon.name == "fileTypes/gitignore" {
        return vec![Variant {
            suffix: "",
            palette: &LIGHT,
        }];
    }
    vec![
        Variant {
            suffix: "",
            palette: &LIGHT,
        },
        Variant {
            suffix: "_dark",
            palette: &DARK,
        },
    ]
}

fn render_icon(icon: &Icon, palette: &Palette, source_dir: &Path) -> Result<String> {
    let color = paint_color(icon.paint, palette);
    match icon.shape {
        Shape::Lucide { name, tint } => {
            let path = source_dir.join(format!("{name}.svg"));
            let source = fs::read_to_string(&path)
                .map_err(|_| format!("Lucide icon {name} not found at {}", path.display()))?;
            render_lucide(name, &source, icon.size, color, tint)
        }
        Shape::Badge(letter) => Ok(render_badge(letter, icon.size, color, palette.over_accent)),
        Shape::Status(status) => Ok(render_status(status, icon.size, color, palette.over_accent)),
    }
}

fn paint_color(paint: Paint, palette: &Palette) -> &'static str {
    match paint {
        Paint::Main => palette.main,
        Paint::Accent => palette.accent,
        Paint::OverAccent => palette.over_accent,
        Paint::Status(Status::Error) => palette.error,
        Paint::Status(Status::Warning) => palette.warning,
        Paint::Status(Status::Success) => palette.success,
        Paint::Status(Status::Info) => palette.info,
        Paint::Kind(Kind::Class) => palette.class,
        Paint::Kind(Kind::Interface) => palette.interface,
        Paint::Kind(Kind::Enum) => palette.enumeration,
        Paint::Kind(Kind::Record) => palette.record,
        Paint::Kind(Kind::Method) => palette.method,
        Paint::Kind(Kind::Field) => palette.field,
        Paint::Kind(Kind::Constant) => palette.constant,
        Paint::Kind(Kind::Package) => palette.package,
        Paint::Kind(Kind::Module) => palette.module,
        Paint::Kind(Kind::Library) => palette.library,
        Paint::Kind(Kind::Folder) => palette.folder,
        Paint::Kind(Kind::SourceRoot) => palette.source_root,
        Paint::File(FileKind::Ignored) => palette.file_ignored,
        Paint::File(FileKind::Module) => palette.file_module,
        Paint::File(FileKind::Source) => palette.file_source,
        Paint::File(FileKind::Text) => palette.file_text,
        Paint::File(FileKind::Xml) => palette.file_xml,
    }
}

/// Keeps the Lucide viewBox of 24 units, so the stroke stays proportional, and
/// replaces its `currentColor` with the fixed colour of the theme.
fn render_lucide(name: &str, source: &str, size: u32, color: &str, tint: bool) -> Result<String> {
    let start = source
        .find("<svg")
        .ok_or_else(|| format!("Lucide icon {name} has no <svg>"))?;
    let body_start = start
        + source[start..]
            .find('>')
            .ok_or_else(|| format!("Lucide icon {name} has no end of <svg>"))?
        + 1;
    let body_end = source
        .rfind("</svg>")
        .ok_or_else(|| format!("Lucide icon {name} has no </svg>"))?;
    let mut body = String::new();
    let mut tint_left = tint;
    for line in source[body_start..body_end].lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut line = line.replace("stroke=\"currentColor\"", &format!("stroke=\"{color}\""));
        line = line.replace("fill=\"currentColor\"", &format!("fill=\"{color}\""));
        if tint_left && line.starts_with("<path") {
            line = line.replacen(
                "/>",
                &format!(" fill=\"{color}\" fill-opacity=\"0.18\"/>"),
                1,
            );
            tint_left = false;
        }
        body.push_str("  ");
        body.push_str(&line);
        body.push('\n');
    }
    if tint && tint_left {
        return Err(format!("Lucide icon {name} has no path to tint").into());
    }
    Ok(format!(
        "<!-- Arro icon. Shape from Lucide {name} (ISC), see LUCIDE-LICENSE.txt. -->\n\
         <svg width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" \
         xmlns=\"http://www.w3.org/2000/svg\" stroke=\"{color}\" stroke-width=\"1.5\" \
         stroke-linecap=\"round\" stroke-linejoin=\"round\">\n\
         {body}</svg>\n"
    ))
}

fn render_badge(letter: char, size: u32, fill: &str, ink: &str) -> String {
    format!(
        "<!-- Arro icon, own drawing. -->\n\
         <svg width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" \
         xmlns=\"http://www.w3.org/2000/svg\">\n\
         \x20 <rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"4.5\" fill=\"{fill}\"/>\n\
         \x20 <path d=\"{}\" stroke=\"{ink}\" stroke-width=\"1.6\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\"/>\n\
         </svg>\n",
        letter_path(letter)
    )
}

fn letter_path(letter: char) -> &'static str {
    match letter {
        'C' => "M15 8H12.5C10.5 8 9.5 9.5 9.5 12C9.5 14.5 10.5 16 12.5 16H15",
        'I' => "M12 8V16",
        'E' => "M15.5 8H9V16H15.5M9 12H14",
        'R' => "M9 16V8H13C15 8 15 11.5 13 11.5H9M12 11.5L15 16",
        'M' => "M9 16V8L12 12L15 8V16",
        'F' => "M9 16V8H15M9 12H14",
        'K' => "M9 8V16M15 8L9.5 12L15 16",
        'G' => "M15 8H12.5C10.5 8 9.5 9.5 9.5 12C9.5 14.5 10.5 16 12.5 16H15.5V12.5H13",
        'J' => "M10.5 8H15.5M13.5 8V13.5C13.5 15.2 12.7 16 11.4 16C10.2 16 9.6 15.3 9.3 14.3",
        'T' => "M9 8H15M12 8V16",
        'X' => "M9 8L15 16M15 8L9 16",
        _ => "M9 8V16",
    }
}

fn render_status(status: Status, size: u32, fill: &str, ink: &str) -> String {
    let sign = match status {
        Status::Error => {
            "<path d=\"M12 7.5v6\" stroke=\"ink\" stroke-width=\"1.8\" stroke-linecap=\"round\"/>\
             <circle cx=\"12\" cy=\"17\" r=\"1.2\" fill=\"ink\"/>"
        }
        Status::Warning => {
            "<path d=\"M12 8v5.5\" stroke=\"ink\" stroke-width=\"1.8\" stroke-linecap=\"round\"/>\
             <circle cx=\"12\" cy=\"16.8\" r=\"1.2\" fill=\"ink\"/>"
        }
        Status::Success => {
            "<path d=\"M8 12.5l3 3 5-6\" stroke=\"ink\" stroke-width=\"1.8\" \
             stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
        }
        Status::Info => {
            "<path d=\"M12 11v5\" stroke=\"ink\" stroke-width=\"1.8\" stroke-linecap=\"round\"/>\
             <circle cx=\"12\" cy=\"8\" r=\"1.2\" fill=\"ink\"/>"
        }
    };
    let sign = sign.replace("ink", ink);
    let shape = match status {
        Status::Warning => format!(
            "<path d=\"M12 3.5L20.5 19H3.5Z\" fill=\"{fill}\" stroke=\"{fill}\" \
             stroke-width=\"1.5\" stroke-linejoin=\"round\"/>"
        ),
        _ => format!("<circle cx=\"12\" cy=\"12\" r=\"9\" fill=\"{fill}\"/>"),
    };
    format!(
        "<!-- Arro icon, own drawing. -->\n\
         <svg width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" \
         xmlns=\"http://www.w3.org/2000/svg\">\n\
         \x20 {shape}\n\
         \x20 {sign}\n\
         </svg>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn expected_files() -> Vec<(String, u32)> {
        let mut files = Vec::new();
        for icon in ICONS {
            files.push((format!("{}.svg", icon.name), icon.size));
            if icon.name != "fileTypes/gitignore" {
                files.push((format!("{}_dark.svg", icon.name), icon.size));
            }
        }
        files
    }

    fn well_formed(text: &str) -> bool {
        let body = match text.find("<svg") {
            Some(index) => text[index..].trim(),
            None => return false,
        };
        body.starts_with("<svg ")
            && body.ends_with("</svg>")
            && body.matches('<').count() == body.matches('>').count()
            && !body.contains("<script")
    }

    #[test]
    fn every_icon_file_exists_and_has_the_expected_shape() {
        let directory = root().join(ICON_DIR);
        let files = expected_files();
        assert_eq!(files.len(), 155);
        for (name, size) in files {
            let path = directory.join(&name);
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(
                text.starts_with("<!-- Arro icon. Shape from Lucide ")
                    || text.starts_with("<!-- Arro icon, own drawing. -->"),
                "{name}: no provenance comment"
            );
            assert!(well_formed(&text), "{name}: not a simple SVG");
            assert!(
                text.contains(&format!("width=\"{size}\"")),
                "{name}: width is not {size}"
            );
            assert!(
                text.contains(&format!("height=\"{size}\"")),
                "{name}: height is not {size}"
            );
            assert!(
                text.contains("viewBox=\"0 0 24 24\""),
                "{name}: no Lucide viewBox"
            );
            assert!(!text.contains("currentColor"), "{name}: uses currentColor");
        }
    }

    #[test]
    fn no_icon_mentions_the_previous_source() {
        let directory = root().join(ICON_DIR);
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                for entry in fs::read_dir(&path).unwrap() {
                    let path = entry.unwrap().path();
                    let text = fs::read_to_string(&path).unwrap();
                    assert!(
                        !text.contains("JetBrains"),
                        "{}: still mentions the previous source",
                        path.display()
                    );
                }
            } else {
                let text = fs::read_to_string(&path).unwrap();
                assert!(
                    !text.contains("JetBrains"),
                    "{}: still mentions the previous source",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn the_table_has_unique_names() {
        let mut names: Vec<&str> = ICONS.iter().map(|icon| icon.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn rendering_the_same_source_twice_gives_the_same_bytes() {
        let source = "<svg width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" \
                      stroke=\"currentColor\" stroke-width=\"2\"><path d=\"M4 4L20 20\"/></svg>";
        let first = render_lucide("sample", source, 16, "#123456", false).unwrap();
        let second = render_lucide("sample", source, 16, "#123456", false).unwrap();
        assert_eq!(first, second);
        assert!(!first.contains("currentColor"));
        assert!(first.starts_with("<!-- Arro icon. Shape from Lucide sample (ISC)"));
    }
}
