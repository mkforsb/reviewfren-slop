//! A forgiving JSON tokenizer: it colors whatever looks like JSON, even
//! when it is truncated, pretty-printed or not quite valid.

use super::{Highlighter, Spans};
use crate::style::theme;
use crate::text::Span;

pub struct Json;

impl Highlighter for Json {
    fn highlight(&self, line: &str) -> Spans {
        let mut out = Spans::new();
        let mut chars = line.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => out.push(Span::string(i, scan_string(&mut chars))),
                '0'..='9' | '-' => out.push(Span::number(i, scan_number(&mut chars))),
                _ => {}
            }
        }
        out
    }
}

fn scan_string(chars: &mut impl Iterator<Item = (usize, char)>) -> usize {
    let mut len = 1;
    for (_, c) in chars {
        len += 1;
        if c == '"' { break; }
    }
    len
}
