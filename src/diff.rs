//! Unified diff parsing and construction of display rows for the code view.

use std::ops::Range;

pub const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

/// Parses the hunks from a `git diff` output for a single file.
pub fn parse_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let (old_start, new_start) = parse_hunk_header(rest);
            let _ = old_start;
            old_no = old_start;
            new_no = new_start;
            hunks.push(Hunk { header: line.to_string(), new_start, lines: Vec::new() });
            continue;
        }
        let Some(h) = hunks.last_mut() else { continue };
        if let Some(t) = line.strip_prefix('+') {
            h.lines.push(DiffLine { kind: LineKind::Added, old_no: None, new_no: Some(new_no), text: t.into() });
            new_no += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            h.lines.push(DiffLine { kind: LineKind::Removed, old_no: Some(old_no), new_no: None, text: t.into() });
            old_no += 1;
        } else if let Some(t) = line.strip_prefix(' ') {
            h.lines.push(DiffLine {
                kind: LineKind::Context,
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: t.into(),
            });
            old_no += 1;
            new_no += 1;
        } else if line.is_empty() {
            // Some tools strip the trailing space of empty context lines.
            h.lines.push(DiffLine { kind: LineKind::Context, old_no: Some(old_no), new_no: Some(new_no), text: String::new() });
            old_no += 1;
            new_no += 1;
        }
        // "\ No newline at end of file" and anything else is ignored.
    }
    hunks
}

/// Parses " -a,b +c,d @@ ..." into (a, c).
fn parse_hunk_header(rest: &str) -> (u32, u32) {
    let mut old = 0;
    let mut new = 0;
    for tok in rest.split_whitespace() {
        if let Some(o) = tok.strip_prefix('-') {
            old = o.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if let Some(n) = tok.strip_prefix('+') {
            new = n.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if tok == "@@" {
            break;
        }
    }
    (old, new)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    HunkHeader,
    Context,
    Added,
    Removed,
}

/// A row to display: either a hunk header or a code line.
#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Text with tabs expanded (what is drawn).
    pub display: String,
    /// The original line text (used to map columns for LSP positions).
    pub original: String,
    /// The parts of a changed line that actually changed, as display column
    /// ranges (see [`mark_inline_changes`]).
    pub emph: Vec<Range<usize>>,
}

impl Row {
    fn line(l: &DiffLine) -> Self {
        let kind = match l.kind {
            LineKind::Context => RowKind::Context,
            LineKind::Added => RowKind::Added,
            LineKind::Removed => RowKind::Removed,
        };
        Row { kind, old_no: l.old_no, new_no: l.new_no, display: expand_tabs(&l.text), original: l.text.clone(), emph: Vec::new() }
    }
}

/// Rows for the compact (hunks only) view.
pub fn compact_rows(hunks: &[Hunk]) -> Vec<Row> {
    let mut rows = Vec::new();
    for h in hunks {
        rows.push(Row {
            kind: RowKind::HunkHeader,
            old_no: None,
            new_no: None,
            display: expand_tabs(&h.header),
            original: h.header.clone(),
            emph: Vec::new(),
        });
        rows.extend(h.lines.iter().map(Row::line));
    }
    mark_inline_changes(&mut rows);
    rows
}

/// Rows for the full file view: every line of the new file, with removed
/// lines interleaved where they were deleted.
pub fn full_rows(hunks: &[Hunk], new_text: &str, old_text: &str) -> Vec<Row> {
    let new_lines: Vec<&str> = new_text.lines().collect();
    let old_lines: Vec<&str> = old_text.lines().collect();
    let mut rows = Vec::new();
    let mut next_new = 1u32; // next new-file line to emit
    let mut next_old = 1u32;

    let push_context = |rows: &mut Vec<Row>, new_no: u32, old_no: u32| {
        let text = new_lines.get(new_no as usize - 1).copied().unwrap_or_default();
        rows.push(Row {
            kind: RowKind::Context,
            old_no: Some(old_no),
            new_no: Some(new_no),
            display: expand_tabs(text),
            original: text.to_string(),
            emph: Vec::new(),
        });
    };

    for h in hunks {
        // Unchanged lines before the hunk.
        let hunk_new_start = if h.new_start == 0 { 1 } else { h.new_start };
        while next_new < hunk_new_start && (next_new as usize) <= new_lines.len() {
            push_context(&mut rows, next_new, next_old);
            next_new += 1;
            next_old += 1;
        }
        for l in &h.lines {
            rows.push(Row::line(l));
            if let Some(n) = l.new_no {
                next_new = n + 1;
            }
            if let Some(o) = l.old_no {
                next_old = o + 1;
            }
        }
    }
    while (next_new as usize) <= new_lines.len() {
        push_context(&mut rows, next_new, next_old);
        next_new += 1;
        next_old += 1;
    }
    // A deleted file has no new lines: show all old lines as removed.
    if new_lines.is_empty() && hunks.is_empty() {
        for (i, l) in old_lines.iter().enumerate() {
            rows.push(Row {
                kind: RowKind::Removed,
                old_no: Some(i as u32 + 1),
                new_no: None,
                display: expand_tabs(l),
                original: l.to_string(),
                emph: Vec::new(),
            });
        }
    }
    mark_inline_changes(&mut rows);
    rows
}

/// Change blocks with more lines than this are rewrites; they get no word highlights.
const INLINE_MAX_BLOCK_LINES: usize = 400;
/// A changed line gets word highlights only if at least this share of its word
/// characters is unchanged. Below that, the line is new or rewritten, and
/// highlighted fragments wouldn't help. Punctuation doesn't count: it matches by
/// accident too easily.
const INLINE_MIN_UNCHANGED: f32 = 0.2;

/// Marks the parts of changed lines that actually changed, like GitHub's darker
/// word highlights.
///
/// Each block of removed lines followed by added lines is diffed as a whole, token
/// by token (words, runs of whitespace, single other characters), so a line split
/// in two or merged still lines up. Tiny unchanged gaps between two changes (up to
/// two spaces or punctuation characters, like the ", " in "a, b" -> "c, d") count
/// as changed, so a highlight doesn't break into confetti.
pub fn mark_inline_changes(rows: &mut [Row]) {
    let mut i = 0;
    while i < rows.len() {
        if rows[i].kind != RowKind::Removed {
            i += 1;
            continue;
        }
        let removed_end = i + rows[i..].iter().take_while(|r| r.kind == RowKind::Removed).count();
        let added_end = removed_end + rows[removed_end..].iter().take_while(|r| r.kind == RowKind::Added).count();
        if added_end > removed_end && added_end - i <= INLINE_MAX_BLOCK_LINES {
            let (removed, added) = rows[i..added_end].split_at_mut(removed_end - i);
            mark_block(removed, added);
        }
        i = added_end;
    }
}

/// A token of a change block: its text, and its line and char range (`None` for
/// the line breaks between lines).
struct Token<'a> {
    text: &'a str,
    at: Option<(usize, Range<usize>)>,
}

fn tokenize(rows: &[Row]) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    for (li, row) in rows.iter().enumerate() {
        if li > 0 {
            out.push(Token { text: "\n", at: None });
        }
        let s = row.display.as_str();
        let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
        let mut chars = s.char_indices().enumerate().peekable();
        while let Some((ci, (bi, c))) = chars.next() {
            let k = class(c);
            let (mut ce, mut be) = (ci + 1, bi + c.len_utf8());
            if k != 2 {
                while let Some(&(cj, (bj, d))) = chars.peek() {
                    if class(d) != k {
                        break;
                    }
                    (ce, be) = (cj + 1, bj + d.len_utf8());
                    chars.next();
                }
            }
            out.push(Token { text: &s[bi..be], at: Some((li, ci..ce)) });
        }
    }
    out
}

fn mark_block(removed: &mut [Row], added: &mut [Row]) {
    use similar::{Algorithm, DiffTag};
    let (old, new) = (tokenize(removed), tokenize(added));
    let old_text: Vec<&str> = old.iter().map(|t| t.text).collect();
    let new_text: Vec<&str> = new.iter().map(|t| t.text).collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let ops = similar::capture_diff_slices_deadline(Algorithm::Histogram, &old_text, &new_text, Some(deadline));

    let mut old_changed = vec![false; old.len()];
    let mut new_changed = vec![false; new.len()];
    for (k, op) in ops.iter().enumerate() {
        let (tag, o, n) = op.as_tag_tuple();
        let changed = match tag {
            DiffTag::Equal => {
                // At most two characters of spaces or punctuation between two changes
                // on the same line.
                let between = k > 0 && k + 1 < ops.len();
                let gap = &old[o.clone()];
                let chars = gap.iter().map(|t| t.text.chars().count()).sum::<usize>();
                let tiny = chars <= 2 && gap.iter().all(|t| t.at.is_some() && !t.text.chars().any(|c| c.is_alphanumeric() || c == '_'));
                between && tiny
            }
            _ => true,
        };
        if changed {
            old_changed[o].fill(true);
            new_changed[n].fill(true);
        }
    }
    let old_emph = line_ranges(removed.len(), &old, &old_changed);
    let new_emph = line_ranges(added.len(), &new, &new_changed);
    for (row, emph) in removed.iter_mut().zip(old_emph).chain(added.iter_mut().zip(new_emph)) {
        row.emph = emph;
    }
}

/// The changed ranges of each line, or none for a line that is mostly changed.
fn line_ranges(lines: usize, tokens: &[Token<'_>], changed: &[bool]) -> Vec<Vec<Range<usize>>> {
    let mut per_line: Vec<Vec<Range<usize>>> = vec![Vec::new(); lines];
    let mut words = vec![(0usize, 0usize); lines]; // (unchanged, total) word chars
    for (t, &ch) in tokens.iter().zip(changed) {
        let Some((li, r)) = &t.at else { continue };
        let n = t.text.chars().filter(|c| c.is_alphanumeric() || *c == '_').count();
        words[*li].1 += n;
        if !ch {
            words[*li].0 += n;
            continue;
        }
        match per_line[*li].last_mut() {
            Some(last) if last.end == r.start => last.end = r.end,
            _ => per_line[*li].push(r.clone()),
        }
    }
    per_line
        .into_iter()
        .zip(words)
        .map(|(ranges, (unchanged, total))| {
            let similar_enough = total > 0 && unchanged as f32 >= total as f32 * INLINE_MIN_UNCHANGED;
            if similar_enough { ranges } else { Vec::new() }
        })
        .collect()
}

pub fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = TAB_WIDTH - (col % TAB_WIDTH);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Maps a display column (chars, tabs expanded) back to a char index in the original text.
pub fn display_col_to_char(original: &str, display_col: usize) -> usize {
    let mut col = 0;
    for (i, c) in original.chars().enumerate() {
        let w = if c == '\t' { TAB_WIDTH - (col % TAB_WIDTH) } else { 1 };
        if display_col < col + w {
            return i;
        }
        col += w;
    }
    original.chars().count()
}

/// Converts a char index into a UTF-16 code unit offset (LSP default encoding).
pub fn char_to_utf16(original: &str, char_idx: usize) -> u32 {
    original.chars().take(char_idx).map(|c| c.len_utf16() as u32).sum()
}

/// Converts a char index into a display column (tabs expanded).
#[cfg(test)]
pub fn char_to_display_col(original: &str, char_idx: usize) -> usize {
    let mut col = 0;
    for c in original.chars().take(char_idx) {
        col += if c == '\t' { TAB_WIDTH - (col % TAB_WIDTH) } else { 1 };
    }
    col
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -6,2 +6,3 @@ fn x\n f\n+g2\n g\n";

    #[test]
    fn parse() {
        let h = parse_hunks(DIFF);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].lines.len(), 4);
        assert_eq!(h[1].new_start, 6);
        assert_eq!(h[1].lines[1].new_no, Some(7));
        assert_eq!(h[1].lines[2].old_no, Some(7));
    }

    #[test]
    fn full() {
        let h = parse_hunks(DIFF);
        let new = "a\nB\nc\nd\ne\nf\ng2\ng\nh\n";
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let rows = full_rows(&h, new, old);
        let texts: Vec<_> = rows.iter().map(|r| r.display.as_str()).collect();
        assert_eq!(texts, ["a", "b", "B", "c", "d", "e", "f", "g2", "g", "h"]);
        assert_eq!(rows[9].old_no, Some(8));
        assert_eq!(rows[9].new_no, Some(9));
    }

    /// Diffs one removed and one added line; returns their highlighted parts.
    fn inline(old: &str, new: &str) -> (Vec<String>, Vec<String>) {
        let diff = format!("@@ -1 +1 @@\n-{}\n+{}\n", old.replace('\n', "\n-"), new.replace('\n', "\n+"));
        let rows = compact_rows(&parse_hunks(&diff));
        let parts = |kind: RowKind| -> Vec<String> {
            rows.iter()
                .filter(|r| r.kind == kind)
                .flat_map(|r| r.emph.iter().map(|e| r.display.chars().skip(e.start).take(e.len()).collect::<String>()))
                .collect()
        };
        (parts(RowKind::Removed), parts(RowKind::Added))
    }

    #[test]
    fn inline_changes_highlight_changed_words() {
        assert_eq!(inline("use super::{Highlighter, Spans};", "use super::{Highlighter, SpanSet};"), (vec!["Spans".into()], vec!["SpanSet".into()]));
        assert_eq!(inline("let x = foo(a, b);", "let x = foo(a, c);"), (vec!["b".into()], vec!["c".into()]));
    }

    #[test]
    fn inline_changes_merge_across_tiny_gaps() {
        // "a, b" -> "c, d": the ", " between two changes is part of the change.
        let (old, new) = inline("call(a, b);", "call(c, d);");
        assert_eq!(old, ["a, b"]);
        assert_eq!(new, ["c, d"]);
    }

    #[test]
    fn inline_changes_on_heavily_changed_lines() {
        let (old, new) = inline("let mut out = Spans::new();", "let mut out = SpanSet::with_capacity(line.len() / 4);");
        assert_eq!(old, ["Spans::new("]);
        assert_eq!(new, ["SpanSet::with_capacity(line.len() / 4"]);
    }

    #[test]
    fn inline_changes_skip_rewritten_lines() {
        assert_eq!(inline("let total = items.len();", "return Err(\"nope\".into());"), (vec![], vec![]));
    }

    #[test]
    fn inline_changes_pure_insertions() {
        // An added argument; nothing was removed from the old line.
        let (old, new) = inline("fn f(a: u32) {", "fn f(a: u32, b: u32) {");
        assert!(old.is_empty());
        assert_eq!(new, [", b: u32"]);
    }

    #[test]
    fn inline_changes_across_lines() {
        // Two removed lines, one modified added line plus a new one.
        let (old, new) = inline("let a = 1;\nlet b = 2;", "let a = 10;\nlet b = 2;\nlet c = 3;");
        assert_eq!(old, ["1"]);
        assert_eq!(new, ["10"]);
    }

    #[test]
    fn tabs() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(display_col_to_char("\tx", 4), 1);
        assert_eq!(display_col_to_char("\tx", 2), 0);
        assert_eq!(char_to_display_col("\tx", 1), 4);
    }
}
