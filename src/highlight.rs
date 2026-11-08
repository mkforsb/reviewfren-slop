//! Syntax highlighting with syntect (syntaxes and themes bundled by `two-face`).

use iced::Color;
use std::ops::Range;
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static THEMES: LazyLock<two_face::theme::EmbeddedLazyThemeSet> =
    LazyLock::new(two_face::theme::extra);

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    /// Range in chars of the (tab-expanded) display text.
    pub range: Range<usize>,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

pub type LineSpans = Vec<Span>;

fn theme(dark: bool) -> &'static Theme {
    use two_face::theme::EmbeddedThemeName as N;
    THEMES.get(if dark { N::OneHalfDark } else { N::InspiredGithub })
}

fn syntax_for(path: &str, first_line: &str) -> &'static SyntaxReference {
    let ss = &*SYNTAXES;
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let ext = file_name.rsplit_once('.').map(|(_, e)| e).unwrap_or(file_name);
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(file_name))
        .or_else(|| match ext {
            "rhai" => ss.find_syntax_by_extension("rs"),
            _ => None,
        })
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Highlights a whole file, returning spans per line (tabs expanded).
///
/// Highlighting the entire file (rather than only diff hunks) keeps multi-line
/// constructs such as block comments and strings correct.
pub fn highlight_file(path: &str, text: &str, dark: bool) -> Vec<LineSpans> {
    let syntax = syntax_for(path, text.lines().next().unwrap_or_default());
    let mut h = HighlightLines::new(syntax, theme(dark));
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(text) {
        let ranges = match h.highlight_line(line, &SYNTAXES) {
            Ok(r) => r,
            Err(_) => {
                out.push(Vec::new());
                continue;
            }
        };
        let mut spans = Vec::new();
        let mut col = 0usize;
        for (style, piece) in ranges {
            let piece = piece.trim_end_matches(['\n', '\r']);
            if piece.is_empty() {
                continue;
            }
            let start = col;
            for c in piece.chars() {
                col += if c == '\t' { crate::diff::TAB_WIDTH - (col % crate::diff::TAB_WIDTH) } else { 1 };
            }
            let fg = style.foreground;
            spans.push(Span {
                range: start..col,
                color: Color::from_rgba8(fg.r, fg.g, fg.b, fg.a as f32 / 255.0),
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            });
        }
        out.push(spans);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rust_highlight() {
        let s = super::highlight_file("a.rs", "fn main() {\n\tlet x = 1;\n}\n", false);
        assert_eq!(s.len(), 3);
        assert!(s[0].len() > 1);
        assert_eq!(s[1][0].range.start, 0);
    }
}
