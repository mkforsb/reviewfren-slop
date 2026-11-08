//! Rich text that can be selected with the mouse and copied with Ctrl+C.
//!
//! iced's `rich_text` can't be selected. This widget shows the same spans, laid out
//! as one paragraph per hard line: a paragraph's hit test only reports a byte offset
//! within its line, so splitting at `\n` keeps hit positions unambiguous. The
//! selection is drawn behind the text and covers one widget, such as a markdown
//! paragraph or a whole code block.
//!
//! Drag to select, double-click selects a word, triple-click a line, Shift+click
//! extends, Ctrl+C copies, Esc clears. Links still work when clicked.

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self, Paragraph as _, Span};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Shell, mouse};
use iced::{Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Theme, Vector, alignment, keyboard};
use std::time::{Duration, Instant};

type Spans<Link> = Vec<Span<'static, Link, Font>>;

pub struct SelectableText<'a, Link, Message> {
    spans: Spans<Link>,
    size: Option<Pixels>,
    font: Option<Font>,
    on_link_click: Option<Box<dyn Fn(Link) -> Message + 'a>>,
}

impl<'a, Link, Message> SelectableText<'a, Link, Message>
where
    Link: Clone + PartialEq + 'static,
{
    pub fn new(spans: impl IntoIterator<Item = Span<'static, Link, Font>>) -> Self {
        Self { spans: spans.into_iter().collect(), size: None, font: None, on_link_click: None }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    pub fn on_link_click(mut self, f: impl Fn(Link) -> Message + 'a) -> Self {
        self.on_link_click = Some(Box::new(f));
        self
    }
}

/// A position in the text: hard line and byte offset within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
struct Pos {
    line: usize,
    byte: usize,
}

/// What the paragraphs were laid out with; a change means laying them out again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Params {
    font: Font,
    size: Pixels,
    width: f32,
}

struct Line<Link, P> {
    spans: Spans<Link>,
    text: String,
    paragraph: P,
    y: f32,
    height: f32,
}

struct State<Link, P> {
    source: Spans<Link>,
    params: Option<Params>,
    lines: Vec<Line<Link, P>>,
    anchor: Option<Pos>,
    head: Option<Pos>,
    dragging: bool,
    focused: bool,
    last_click: Option<(Instant, Pos, u8)>,
    modifiers: keyboard::Modifiers,
    /// Selection highlight, relative to the widget's top-left corner.
    selection_rects: Vec<Rectangle>,
    hovered_link: Option<(usize, usize)>,
    pressed_link: Option<(usize, usize)>,
}

impl<Link, P> Default for State<Link, P> {
    fn default() -> Self {
        Self {
            source: Vec::new(),
            params: None,
            lines: Vec::new(),
            anchor: None,
            head: None,
            dragging: false,
            focused: false,
            last_click: None,
            modifiers: keyboard::Modifiers::default(),
            selection_rects: Vec::new(),
            hovered_link: None,
            pressed_link: None,
        }
    }
}

fn build<Link, P: text::Paragraph<Font = Font>>(spans: &[Span<'static, Link, Font>], params: Params) -> P {
    P::with_spans(text::Text {
        content: spans,
        bounds: Size::new(params.width, f32::INFINITY),
        size: params.size,
        line_height: text::LineHeight::default(),
        font: params.font,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::Word,
    })
}

/// Splits spans into hard lines at `\n`.
fn split_lines<Link: Clone>(spans: &[Span<'static, Link, Font>]) -> Vec<Spans<Link>> {
    let mut lines = vec![Vec::new()];
    for span in spans {
        for (k, piece) in span.text.split('\n').enumerate() {
            if k > 0 {
                lines.push(Vec::new());
            }
            if !piece.is_empty() {
                let last = lines.last_mut().expect("at least one line");
                last.push(Span { text: piece.to_string().into(), ..span.clone() });
            }
        }
    }
    lines
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Byte range of the word around `byte`, or of the single character there.
fn word_at(s: &str, byte: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before = s[..byte].chars().next_back();
    let after = s[byte..].chars().next();
    if after.is_some_and(is_word) || before.is_some_and(is_word) {
        let start = s[..byte].char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(byte, |(i, _)| i);
        let end = s[byte..].char_indices().find(|&(_, c)| !is_word(c)).map_or(s.len(), |(i, _)| byte + i);
        (start, end)
    } else {
        (byte, after.map_or(byte, |c| byte + c.len_utf8()))
    }
}

impl<Link: Clone, P: text::Paragraph<Font = Font>> State<Link, P> {
    fn range(&self) -> Option<(Pos, Pos)> {
        let (a, h) = (self.anchor?, self.head?);
        if a == h { None } else { Some((a.min(h), a.max(h))) }
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
        self.head = None;
        self.selection_rects.clear();
    }

    fn relayout(&mut self, params: Params) {
        self.lines = split_lines(&self.source)
            .into_iter()
            .map(|spans| {
                let text: String = spans.iter().map(|s| s.text.as_ref()).collect();
                let paragraph: P = build(&spans, params);
                Line { spans, text, paragraph, y: 0.0, height: 0.0 }
            })
            .collect();
        let empty_height = text::LineHeight::default().to_absolute(params.size).0;
        let mut y = 0.0;
        for l in &mut self.lines {
            l.y = y;
            l.height = if l.text.is_empty() { empty_height } else { l.paragraph.min_bounds().height.max(empty_height) };
            y += l.height;
        }
        // Keep the selection valid if the text changed (e.g. while a reply streams in).
        let clamp = |p: Pos, lines: &[Line<Link, P>]| -> Pos {
            let line = p.line.min(lines.len().saturating_sub(1));
            let text = lines.get(line).map_or("", |l| l.text.as_str());
            Pos { line, byte: floor_boundary(text, p.byte) }
        };
        self.anchor = self.anchor.map(|p| clamp(p, &self.lines));
        self.head = self.head.map(|p| clamp(p, &self.lines));
        self.params = Some(params);
        self.update_selection_rects();
    }

    fn height(&self) -> f32 {
        self.lines.last().map_or(0.0, |l| l.y + l.height)
    }

    /// Text position under `p` (relative to the widget), clamped to the text.
    fn pos_at(&self, p: Point) -> Pos {
        let Some(last) = self.lines.len().checked_sub(1) else { return Pos::default() };
        if p.y < 0.0 {
            return Pos { line: 0, byte: 0 };
        }
        if p.y >= self.height() {
            return Pos { line: last, byte: self.lines[last].text.len() };
        }
        let li = self.lines.iter().position(|l| p.y < l.y + l.height).unwrap_or(last);
        let line = &self.lines[li];
        let local = Point::new(p.x.max(0.0), (p.y - line.y).max(0.0));
        let byte = match line.paragraph.hit_test(local) {
            Some(hit) => hit.cursor(),
            None if p.x <= 0.0 => 0,
            None => line.text.len(),
        };
        Pos { line: li, byte: floor_boundary(&line.text, byte) }
    }

    /// The link span under `p` (relative to the widget), as (line, span).
    fn link_at(&self, p: Point) -> Option<(usize, usize)> {
        let li = self.lines.iter().position(|l| p.y >= l.y && p.y < l.y + l.height)?;
        let line = &self.lines[li];
        let span = line.paragraph.hit_span(Point::new(p.x, p.y - line.y))?;
        line.spans.get(span)?.link.as_ref().map(|_| (li, span))
    }

    fn selected_text(&self) -> Option<String> {
        let (s, e) = self.range()?;
        let mut out = String::new();
        for li in s.line..=e.line {
            let text = &self.lines[li].text;
            let a = if li == s.line { s.byte } else { 0 };
            let b = if li == e.line { e.byte } else { text.len() };
            if li > s.line {
                out.push('\n');
            }
            out.push_str(&text[a..b.max(a)]);
        }
        Some(out)
    }

    fn update_selection_rects(&mut self) {
        self.selection_rects.clear();
        let (Some((s, e)), Some(params)) = (self.range(), self.params) else { return };
        for li in s.line..=e.line {
            let line = &self.lines[li];
            let offset = Vector::new(0.0, line.y);
            let a = if li == s.line { s.byte } else { 0 };
            let b = if li == e.line { e.byte } else { line.text.len() };
            if line.text.is_empty() && li < e.line {
                // An empty line inside the selection: mark its newline.
                let w = params.size.0 * 0.4;
                self.selection_rects.push(Rectangle::new(Point::new(0.0, line.y), Size::new(w, line.height)));
                continue;
            }
            if a >= b {
                continue;
            }
            if a == 0 && b == line.text.len() {
                // Whole line: the laid-out paragraph already has the bounds.
                for i in 0..line.spans.len() {
                    self.selection_rects.extend(line.paragraph.span_bounds(i).into_iter().map(|r| r + offset));
                }
                continue;
            }
            // Part of a line: lay it out again with the spans split at the selection
            // edges, and take the bounds of the selected pieces.
            let mut pieces = Vec::new();
            let mut selected = Vec::new();
            let mut start = 0;
            for span in &line.spans {
                let end = start + span.text.len();
                let (sa, sb) = (a.clamp(start, end), b.clamp(start, end));
                for (x0, x1, sel) in [(start, sa, false), (sa, sb, true), (sb, end, false)] {
                    if x1 > x0 {
                        if sel {
                            selected.push(pieces.len());
                        }
                        pieces.push(Span { text: span.text[x0 - start..x1 - start].to_string().into(), ..span.clone() });
                    }
                }
                start = end;
            }
            let paragraph: P = build(&pieces, params);
            for i in selected {
                self.selection_rects.extend(paragraph.span_bounds(i).into_iter().map(|r| r + offset));
            }
        }
    }
}

impl<Link, Message, Renderer> Widget<Message, Theme, Renderer> for SelectableText<'_, Link, Message>
where
    Link: Clone + PartialEq + 'static,
    Renderer: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link, Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link, Renderer::Paragraph>::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Link, Renderer::Paragraph>>();
        let params = Params {
            font: self.font.unwrap_or_else(|| renderer.default_font()),
            size: self.size.unwrap_or_else(|| renderer.default_size()),
            width: limits.max().width,
        };
        if state.source != self.spans || state.params != Some(params) {
            state.source = self.spans.clone();
            state.relayout(params);
        }
        let width = state.lines.iter().map(|l| l.paragraph.min_bounds().width).fold(0.0, f32::max);
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, Size::new(width, state.height())))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State<Link, Renderer::Paragraph>>();
        let bounds = layout.bounds();
        let relative = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y);

        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(mods)) => state.modifiers = *mods,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_over(bounds) {
                    let p = relative(p);
                    if self.on_link_click.is_some()
                        && let Some(link) = state.link_at(p)
                    {
                        state.pressed_link = Some(link);
                        shell.capture_event();
                        return;
                    }
                    let pos = state.pos_at(p);
                    let now = Instant::now();
                    let clicks = match state.last_click {
                        Some((t, prev, n)) if now - t < Duration::from_millis(400) && prev.line == pos.line => n % 3 + 1,
                        _ => 1,
                    };
                    state.last_click = Some((now, pos, clicks));
                    let text = &state.lines.get(pos.line).map_or("", |l| l.text.as_str());
                    match clicks {
                        2 => {
                            let (a, b) = word_at(text, pos.byte);
                            state.anchor = Some(Pos { line: pos.line, byte: a });
                            state.head = Some(Pos { line: pos.line, byte: b });
                            state.dragging = false;
                        }
                        3 => {
                            state.anchor = Some(Pos { line: pos.line, byte: 0 });
                            state.head = Some(Pos { line: pos.line, byte: text.len() });
                            state.dragging = false;
                        }
                        _ => {
                            if !(state.modifiers.shift() && state.anchor.is_some()) {
                                state.anchor = Some(pos);
                            }
                            state.head = Some(pos);
                            state.dragging = true;
                        }
                    }
                    state.focused = true;
                    state.update_selection_rects();
                    shell.capture_event();
                    shell.request_redraw();
                } else if state.anchor.is_some() || state.focused {
                    state.clear_selection();
                    state.focused = false;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.dragging {
                    // The cursor is adjusted for the scroll offset of an enclosing
                    // scrollable even when it has left it.
                    if let Some(p) = cursor.land().position() {
                        let head = Some(state.pos_at(relative(p)));
                        if head != state.head {
                            state.head = head;
                            state.update_selection_rects();
                            shell.request_redraw();
                        }
                    }
                } else if self.on_link_click.is_some() {
                    let hovered = cursor.position_over(bounds).and_then(|p| state.link_at(relative(p)));
                    if hovered != state.hovered_link {
                        state.hovered_link = hovered;
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
                if let Some(pressed) = state.pressed_link.take()
                    && let Some(p) = cursor.position_over(bounds)
                    && state.link_at(relative(p)) == Some(pressed)
                    && let Some(f) = &self.on_link_click
                    && let Some(link) = state.lines[pressed.0].spans[pressed.1].link.clone()
                {
                    shell.publish(f(link));
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) if state.focused => {
                if modifiers.command() && matches!(key.as_ref(), keyboard::Key::Character("c")) {
                    if let Some(text) = state.selected_text() {
                        clipboard.write(clipboard::Kind::Standard, text);
                        shell.capture_event();
                    }
                } else if matches!(key.as_ref(), keyboard::Key::Named(keyboard::key::Named::Escape)) && state.anchor.is_some() {
                    state.clear_selection();
                    shell.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State<Link, Renderer::Paragraph>>();
        let bounds = layout.bounds();
        match cursor.position_over(bounds) {
            Some(_) if state.hovered_link.is_some() => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Text,
            None if state.dragging => mouse::Interaction::Text,
            None => mouse::Interaction::None,
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State<Link, Renderer::Paragraph>>();
        let Some(params) = state.params else { return };
        let origin = Vector::new(bounds.x, bounds.y);
        let quad = |renderer: &mut Renderer, bounds: Rectangle, border: iced::Border, bg: iced::Background| {
            renderer.fill_quad(renderer::Quad { bounds, border, ..Default::default() }, bg);
        };

        // Span backgrounds (inline code) go first, then the selection, then the text.
        for line in &state.lines {
            for (i, span) in line.spans.iter().enumerate() {
                if let Some(h) = span.highlight {
                    for r in line.paragraph.span_bounds(i) {
                        let r = Rectangle::new(
                            r.position() - Vector::new(span.padding.left, span.padding.top),
                            r.size() + Size::new(span.padding.x(), span.padding.y()),
                        );
                        quad(renderer, r + origin + Vector::new(0.0, line.y), h.border, h.background);
                    }
                }
            }
        }
        let primary = theme.palette().primary;
        let selection = Color { a: 0.3, ..primary };
        for r in &state.selection_rects {
            quad(renderer, *r + origin, iced::Border::default(), selection.into());
        }
        for (li, line) in state.lines.iter().enumerate() {
            let top = bounds.y + line.y;
            if top > viewport.y + viewport.height || top + line.height < viewport.y {
                continue;
            }
            renderer.fill_paragraph(&line.paragraph, Point::new(bounds.x, top), style.text_color, *viewport);

            // Underlines (links on hover) and strikethrough.
            for (i, span) in line.spans.iter().enumerate() {
                let hovered = state.hovered_link == Some((li, i));
                if !(span.underline || span.strikethrough || hovered) {
                    continue;
                }
                let size = span.size.unwrap_or(params.size).0;
                let lh = span.line_height.unwrap_or_default().to_absolute(Pixels(size)).0;
                let color = span.color.unwrap_or(style.text_color);
                let baseline = size + (lh - size) / 2.0;
                for r in line.paragraph.span_bounds(i) {
                    let r = r + origin + Vector::new(0.0, line.y);
                    let mut rule = |dy: f32| {
                        let bar = Rectangle::new(Point::new(r.x, r.y + dy), Size::new(r.width, 1.0));
                        quad(renderer, bar, iced::Border::default(), color.into());
                    };
                    if span.underline || hovered {
                        rule(baseline - size * 0.08);
                    }
                    if span.strikethrough {
                        rule(baseline - size / 2.0);
                    }
                }
            }
        }
    }
}

impl<'a, Link, Message, Renderer> From<SelectableText<'a, Link, Message>> for Element<'a, Message, Theme, Renderer>
where
    Link: Clone + PartialEq + 'static,
    Message: 'a,
    Renderer: text::Renderer<Font = Font> + 'a,
{
    fn from(t: SelectableText<'a, Link, Message>) -> Self {
        Element::new(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_spans_into_lines() {
        let spans: Spans<()> = vec![Span::new("a\nb"), Span::new("c"), Span::new("\n\nd")];
        let lines = split_lines(&spans);
        let texts: Vec<String> = lines.iter().map(|l| l.iter().map(|s| s.text.as_ref()).collect()).collect();
        assert_eq!(texts, ["a", "bc", "", "d"]);
    }

    #[test]
    fn words() {
        assert_eq!(word_at("let foo_bar = 1;", 6), (4, 11));
        assert_eq!(word_at("let foo_bar = 1;", 4), (4, 11));
        assert_eq!(word_at("a = b", 2), (2, 3));
        assert_eq!(word_at("åäö x", 2), (0, 6));
    }
}
