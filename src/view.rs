//! All screens and overlays.

use crate::app::*;
use crate::claude;
use crate::fonts;
use crate::code_view::{CodeView, Colors, Insert};
use crate::git::{ChangeKind, PullRequest, ReviewComment};
use crate::mason::Server;
use crate::selectable_text::SelectableText;
use crate::style::{self, small};
use iced::widget::text_editor::{Binding, KeyPress};
use iced::widget::{
    Space, button, center, checkbox, column, combo_box, container, markdown, mouse_area, opaque, pick_list, row, scrollable,
    slider, stack, text, text_editor, text_input, toggler, tooltip,
};
use iced::{Alignment, Color, Element, Fill, Font, Length, Padding, Theme};
use std::collections::BTreeMap;

type El<'a> = Element<'a, Message>;

/// What markdown views need: the markdown settings, and the text of the most
/// recent copy so its button can say "Copied!" for a moment.
#[derive(Clone, Copy)]
struct Md<'a> {
    settings: markdown::Settings,
    copied: Option<&'a str>,
}

impl Md<'_> {
    fn copy_label(&self, text: &str) -> &'static str {
        if self.copied == Some(text) { "Copied!" } else { "Copy" }
    }
}

/// Markdown viewer with selectable text and GitHub-like code blocks (subtle
/// background, copy button).
struct MdViewer<'a> {
    md: Md<'a>,
}

impl<'a> markdown::Viewer<'a, Message> for MdViewer<'a> {
    fn on_link_click(url: markdown::Uri) -> Message {
        Message::LinkClicked(url)
    }

    fn paragraph(&self, settings: markdown::Settings, text: &markdown::Text) -> El<'a> {
        SelectableText::new(text.spans(settings.style).iter().cloned())
            .size(settings.text_size)
            .on_link_click(Message::LinkClicked)
            .into()
    }

    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> El<'a> {
        use markdown::HeadingLevel as H;
        let size = match level {
            H::H1 => settings.h1_size,
            H::H2 => settings.h2_size,
            H::H3 => settings.h3_size,
            H::H4 => settings.h4_size,
            H::H5 => settings.h5_size,
            H::H6 => settings.h6_size,
        };
        let top = if index > 0 { settings.text_size.0 / 2.0 } else { 0.0 };
        container(SelectableText::new(text.spans(settings.style).iter().cloned()).size(size).on_link_click(Message::LinkClicked))
            .padding(Padding { top, ..Padding::ZERO })
            .into()
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        code: &'a str,
        lines: &'a [markdown::Text],
    ) -> El<'a> {
        // One widget for the whole block, so a selection can span several lines.
        let mut spans = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                spans.push(iced::advanced::text::Span::new("\n"));
            }
            spans.extend(line.spans(settings.style).iter().cloned());
        }
        let body = SelectableText::new(spans).on_link_click(Message::LinkClicked).font(fonts::code()).size(13);
        container(
            column![
                row![
                    Space::new().width(Fill),
                    button(small(self.md.copy_label(code))).padding([0, 6]).style(style::flat_button).on_press(Message::CopyText(code.to_string())),
                ],
                scrollable(container(body).padding(Padding { bottom: 8.0, ..Padding::ZERO }))
                    .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default().width(4).scroller_width(4))),
            ]
            .spacing(2),
        )
        .padding([6, 10])
        .width(Fill)
        .style(|t: &Theme| {
            let mut s = style::card(t);
            s.background = Some(iced::Background::Color(style::subtle_bg(t)));
            s
        })
        .into()
    }
}

fn md_view<'a>(items: &'a [markdown::Item], md: Md<'a>) -> El<'a> {
    markdown::view_with(items, md.settings, &MdViewer { md })
}

impl App {
    pub fn view(&self) -> El<'_> {
        let theme = self.theme();
        let base: El = match &self.screen {
            Screen::RepoSelect(s) => repo_select_view(s, &self.settings.recent_repos),
            Screen::PrList(s) => pr_list_view(s),
            Screen::Loading(l) => loading_view(l),
            Screen::Review(r) => review_view(r, self, &theme),
        };
        let mut layers = vec![base];

        if let Screen::Review(r) = &self.screen {
            if let Some(menu) = &r.context_menu {
                layers.push(context_menu_view(r, menu, self.window_size));
            }
            if let Some(p) = r.claude.permissions.front() {
                layers.push(modal(permission_view(p, r.claude.permissions.len()), None));
            }
        }
        if let Some(ed) = &self.settings_editor {
            layers.push(modal(settings_view(ed, &self.settings.ui_font), Some(Message::CloseSettings)));
        }
        if let Some(err) = &self.modal_error {
            layers.push(modal(error_modal_view(err), Some(Message::DismissModal)));
        }
        stack(layers).width(Fill).height(Fill).into()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn modal<'a>(content: El<'a>, on_blur: Option<Message>) -> El<'a> {
    opaque(mouse_area(center(opaque(content)).style(style::backdrop)).on_press(on_blur.unwrap_or(Message::Noop)))
}

fn btn<'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, Message> {
    button(text(label).size(13)).padding([5, 12]).style(style::default_button)
}

fn primary_btn<'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, Message> {
    button(text(label).size(13)).padding([5, 12]).style(button::primary)
}

fn icon_btn<'a>(label: impl text::IntoFragment<'a>, tip: &'a str, msg: Message) -> El<'a> {
    tooltip(
        button(text(label).size(14)).padding([3, 8]).style(style::flat_button).on_press(msg),
        container(text(tip).size(12)).padding([4, 8]).style(style::menu),
        tooltip::Position::Bottom,
    )
    .into()
}

fn errors_view(errors: &[String]) -> El<'_> {
    column(errors.iter().enumerate().map(|(i, e)| style::error_box(e, Some(Message::DismissError(i)))))
        .spacing(8)
        .into()
}

fn relative_time(iso: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(iso) else { return iso.to_string() };
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds().max(0);
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 30 * 86_400 => (s / 86_400, "day"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month"),
        s => (s / (365 * 86_400), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn settings_gear<'a>() -> El<'a> {
    icon_btn("⚙", "Settings", Message::OpenSettings)
}

// ---------------------------------------------------------------------------
// Repo select
// ---------------------------------------------------------------------------

fn repo_select_view<'a>(s: &'a RepoSelect, recent: &'a [std::path::PathBuf]) -> El<'a> {
    let header = row![text("PR Review").size(28), Space::new().width(Fill), settings_gear()].align_y(Alignment::Center);
    let picker = row![
        primary_btn(if s.busy { "Opening…" } else { "Choose repository…" }).on_press_maybe((!s.busy).then_some(Message::PickRepo)),
        text("or").size(13),
        text_input("/path/to/git/repo", &s.path_input)
            .on_input(Message::RepoPathChanged)
            .on_submit(Message::OpenRepoPath)
            .padding(6)
            .size(13),
        btn("Open").on_press_maybe((!s.busy).then_some(Message::OpenRepoPath)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut content = column![
        header,
        text("Select a local Git repository to review its open pull requests.").size(14),
        picker,
        errors_view(&s.errors),
    ]
    .spacing(16)
    .max_width(820);

    if !recent.is_empty() {
        let list = column(recent.iter().map(|p| {
            button(row![text("›").size(14), text(p.display().to_string()).size(13)].spacing(8))
                .width(Fill)
                .padding([6, 10])
                .style(style::flat_button)
                .on_press(Message::OpenRecent(p.clone()))
                .into()
        }))
        .spacing(2);
        content = content.push(
            container(column![text("Recent repositories").size(14), list].spacing(8))
                .padding(12)
                .width(Fill)
                .style(style::card),
        );
    }
    container(content).padding(40).center_x(Fill).into()
}

// ---------------------------------------------------------------------------
// PR list
// ---------------------------------------------------------------------------

fn pr_icon<'a>(pr: &PullRequest) -> El<'a> {
    let color = if pr.is_draft { Color::from_rgb8(0x59, 0x63, 0x6e) } else { Color::from_rgb8(0x1a, 0x7f, 0x37) };
    text("⇅").size(18).color(color).into()
}

fn pr_row<'a>(pr: &'a PullRequest, busy: Option<u64>) -> El<'a> {
    let mut title_row = row![text(&pr.title).size(16).font(fonts::ui_weight(iced::font::Weight::Semibold))]
        .spacing(8)
        .align_y(Alignment::Center);
    if pr.is_draft {
        title_row = title_row.push(container(small("Draft")).padding([1, 8]).style(style::pill(Color::from_rgb8(0x59, 0x63, 0x6e))));
    }
    for l in &pr.labels {
        let c = u32::from_str_radix(&l.color, 16).ok().map(|v| Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8));
        title_row = title_row.push(container(small(l.name.clone())).padding([1, 8]).style(style::pill(c.unwrap_or(Color::from_rgb8(0x09, 0x69, 0xda)))));
    }
    let mut meta = format!("#{} · {} opened {}", pr.number, pr.author.login, relative_time(&pr.created_at));
    if let Some((i, n)) = pr.stack {
        meta.push_str(&format!(" · ☰ stack {i}/{n}"));
    }
    meta.push_str(&format!(" · {} ← {}", pr.base_ref_name, pr.head_ref_name));
    let stats = row![
        text(format!("+{}", pr.additions)).size(12).style(|t: &Theme| text::Style { color: Some(style::green(t)) }),
        text(format!("−{}", pr.deletions)).size(12).style(|t: &Theme| text::Style { color: Some(style::red(t)) }),
        text(format!("{} files", pr.changed_files)).size(12).style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
    ]
    .spacing(8);
    let right: El = if busy == Some(pr.number) { text("Checking…").size(12).into() } else { stats.into() };
    button(
        row![
            pr_icon(pr),
            column![title_row, text(meta).size(12).style(|t: &Theme| text::Style { color: Some(style::muted(t)) })].spacing(4).width(Fill),
            right,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([10, 16])
    .style(style::flat_button)
    .on_press_maybe(busy.is_none().then_some(Message::SelectPr(pr.number)))
    .into()
}

fn pr_list_view(s: &PrList) -> El<'_> {
    let header = row![
        btn("← Repositories").on_press(Message::BackToRepoSelect),
        column![
            text(s.repo.display().to_string()).size(18),
            small(format!("Current branch: {}", s.branch.as_deref().unwrap_or("?"))),
        ]
        .spacing(2),
        Space::new().width(Fill),
        btn(if s.loading { "Loading…" } else { "↻ Refresh" }).on_press_maybe((!s.loading).then_some(Message::RefreshPrs)),
        settings_gear(),
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    let list_header = container(
        row![
            text("Open").size(14).font(fonts::ui_weight(iced::font::Weight::Bold)),
            container(small(s.prs.len().to_string())).padding([1, 8]).style(style::pill(Color::from_rgb8(0x59, 0x63, 0x6e))),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([12, 16])
    .width(Fill)
    .style(style::card_header);

    let body: El = if s.loading && s.prs.is_empty() {
        container(text("Fetching pull requests with gh…").size(14)).padding(24).into()
    } else if s.prs.is_empty() {
        container(text("There are no open pull requests in this repository.").size(14)).padding(24).into()
    } else {
        column(s.prs.iter().map(|p| pr_row(p, s.busy_pr))).into()
    };
    let list = container(column![list_header, body]).width(Fill).style(style::card);
    scrollable(container(column![header, errors_view(&s.errors), list].spacing(16).max_width(1100)).padding(24).center_x(Fill))
        .width(Fill)
        .height(Fill)
        .into()
}

fn loading_view(l: &Loading) -> El<'_> {
    center(
        column![
            text(format!("PR #{}: {}", l.pr.number, l.pr.title)).size(18),
            text(&l.step).size(14),
            text(&l.log).size(12).font(fonts::code()),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .into()
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

fn review_view<'a>(r: &'a Review, app: &'a App, theme: &Theme) -> El<'a> {
    let stack_info = r.pr.stack.map(|(i, n)| format!(" · ☰ stack {i}/{n}")).unwrap_or_default();
    let top = container(
        row![
            btn("← PRs").on_press(Message::BackToPrList),
            column![
                row![
                    text(&r.pr.title).size(17).font(fonts::ui_weight(iced::font::Weight::Semibold)),
                    text(format!("#{}", r.pr.number)).size(17).style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
                ]
                .spacing(6),
                text(format!(
                    "{} wants to merge into {} from {}{stack_info} · merge base {}",
                    r.pr.author.login,
                    r.pr.base_ref_name,
                    r.pr.head_ref_name,
                    &r.merge_base[..r.merge_base.len().min(10)]
                ))
                .size(12)
                .style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
            ]
            .spacing(2)
            .width(Fill),
            text(format!("{} / {} files viewed", r.viewed_count(), r.files.len())).size(12),
            icon_btn("↗", "Open the PR on GitHub", Message::OpenUrl(r.pr.url.clone())),
            icon_btn("⊞", "Expand all files", Message::ExpandAll),
            icon_btn("⊟", "Collapse all files", Message::CollapseAll),
            pick_list(Server::ALL, None::<Server>, Message::StartLsp).placeholder("LSP").text_size(13).padding([4, 10]).style(style::menu_pick_list),
            icon_btn(if app.settings.show_tree { "◧" } else { "▢" }, "Toggle file tree", Message::ToggleTree),
            icon_btn(if app.settings.show_chat { "◨" } else { "▢" }, "Toggle Claude chat panel", Message::ToggleChat),
            settings_gear(),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .width(Fill)
    .style(style::bar);

    let colors = Colors::new(style::is_dark(theme));
    let md = Md { settings: style::markdown_settings(theme), copied: app.copied() };

    // Main continuous page.
    let mut page = column![].spacing(16).padding(Padding { top: 16.0, right: 20.0, bottom: 200.0, left: 16.0 });
    if !r.errors.is_empty() {
        page = page.push(errors_view(&r.errors));
    }
    page = page.push(description_view(r, md));
    let mut shown = 0;
    for (i, f) in r.files.iter().enumerate() {
        if !r.is_visible(f) {
            continue;
        }
        shown += 1;
        page = page.push(file_section(r, i, f, colors, md, app.settings.wrap_lines));
    }
    if shown == 0 {
        page = page.push(text("No files match the current filter.").size(14));
    }
    let main = scrollable(page).id(REVIEW_SCROLL_ID).width(Fill).height(Fill);

    let mut body = row![].height(Fill);
    if app.settings.show_tree {
        body = body.push(file_tree_view(r));
    }
    body = body.push(main);
    if app.settings.show_chat {
        body = body.push(general_chat_view(r, md));
    }

    column![top, body, status_bar(r)].into()
}

fn description_view<'a>(r: &'a Review, md: Md<'a>) -> El<'a> {
    let header = button(
        row![
            text(if r.show_description { "▾" } else { "▸" }).size(14),
            text(format!("{} commented {}", r.pr.author.login, relative_time(&r.pr.created_at))).size(13),
        ]
        .spacing(8),
    )
    .width(Fill)
    .padding([8, 12])
    .style(style::flat_button)
    .on_press(Message::ToggleDescription);
    let mut col = column![container(header).style(|t: &Theme| {
        let mut s = style::info_box_style(t);
        s.border.width = 0.0;
        s
    })];
    if r.show_description {
        col = col.push(container(md_view(r.pr_body.items(), md)).padding(16));
    }
    container(col).width(Fill).style(style::card).into()
}

fn diff_bar<'a>(adds: u32, dels: u32) -> El<'a> {
    let total = adds + dels;
    let green = if total == 0 { 0 } else { ((adds as f32 / total as f32) * 5.0).round() as u32 };
    let red = if total == 0 { 0 } else { 5 - green };
    let block = |kind: u8| -> El<'a> {
        container(Space::new().width(8).height(8))
            .style(move |t: &Theme| container::Style {
                background: Some(iced::Background::Color(match kind {
                    0 => style::green(t),
                    1 => style::red(t),
                    _ => style::border_color(t),
                })),
                border: iced::Border { radius: 2.0.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    };
    let mut r = row![].spacing(2);
    for k in 0..5 {
        r = r.push(block(if k < green { 0 } else if k < green + red { 1 } else { 2 }));
    }
    r.into()
}

fn file_section<'a>(r: &'a Review, i: usize, f: &'a FileEntry, colors: Colors, md: Md<'a>, wrap_default: bool) -> El<'a> {
    let wrap = f.wraps(wrap_default);
    let file = &f.data.file;
    let collapsed = f.collapsed || f.viewed;
    let path_label = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let (kind_label, kind_color) = match file.kind {
        ChangeKind::Added => ("added", Color::from_rgb8(0x1a, 0x7f, 0x37)),
        ChangeKind::Deleted => ("deleted", Color::from_rgb8(0xcf, 0x22, 0x2e)),
        ChangeKind::Renamed => ("renamed", Color::from_rgb8(0x09, 0x69, 0xda)),
        ChangeKind::Modified => ("", Color::BLACK),
    };
    let mut header = row![
        button(text(if collapsed { "▸" } else { "▾" }).size(14)).padding([2, 6]).style(style::flat_button).on_press(Message::ToggleCollapsed(i)),
        text(path_label).size(13).font(fonts::code()).style(move |t: &Theme| text::Style {
            color: Some(if f.viewed { style::muted(t) } else { t.palette().text })
        }),
        if md.copied == Some(file.path.as_str()) {
            icon_btn("✓", "Copied!", Message::CopyText(file.path.clone()))
        } else {
            icon_btn("⧉", "Copy path", Message::CopyText(file.path.clone()))
        },
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if !kind_label.is_empty() {
        header = header.push(container(small(kind_label)).padding([1, 8]).style(style::pill(kind_color)));
    }
    let comments: usize = f.threads.iter().map(|t| t.thread.comments.len()).sum();
    if comments > 0 {
        let label = if comments == 1 { "1 comment".to_string() } else { format!("{comments} comments") };
        header = header.push(container(small(label)).padding([1, 8]).style(style::pill(Color::from_rgb8(0x59, 0x63, 0x6e))));
    }
    if f.annotations.is_some() {
        header = header.push(container(small("✦ annotated")).padding([1, 8]).style(style::pill(Color::from_rgb8(0x82, 0x50, 0xdf))));
    }
    header = header.push(Space::new().width(Fill));
    header = header.push(
        row![
            text(format!("+{}", file.additions)).size(12).style(|t: &Theme| text::Style { color: Some(style::green(t)) }),
            text(format!("−{}", file.deletions)).size(12).style(|t: &Theme| text::Style { color: Some(style::red(t)) }),
            diff_bar(file.additions, file.deletions),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    );
    if !f.data.binary {
        header = header.push(
            tooltip(
                button(text(if f.show_full { "⇕ Compact" } else { "⇕ Full file" }).size(12))
                    .padding([3, 8])
                    .style(style::default_button)
                    .on_press(Message::ToggleFull(i)),
                container(text(if f.show_full { "Show only the changed hunks" } else { "Expand to the whole file" }).size(12))
                    .padding([4, 8])
                    .style(style::menu),
                tooltip::Position::Bottom,
            ),
        );
        header = header.push(
            tooltip(
                button(text("↩ Wrap").size(12))
                    .padding([3, 8])
                    .style(style::toggle_button(wrap))
                    .on_press(Message::ToggleWrap(i)),
                container(text(if wrap { "Scroll long lines horizontally" } else { "Wrap long lines" }).size(12))
                    .padding([4, 8])
                    .style(style::menu),
                tooltip::Position::Bottom,
            ),
        );
    }
    header = header.push(
        pick_list([ClaudeMenu::Annotate, ClaudeMenu::Chat], None::<ClaudeMenu>, move |c| Message::ClaudeMenu(i, c))
            .placeholder("✦ Claude")
            .text_size(12)
            .padding([3, 8])
            .style(style::menu_pick_list),
    );
    header = header.push(
        container(checkbox(f.viewed).label("Viewed").text_size(12).size(14).on_toggle(move |v| Message::ToggleViewed(i, v)))
            .padding([3, 8])
            .style(|t: &Theme| {
                let mut s = style::card(t);
                s.background = Some(iced::Background::Color(style::subtle_bg(t)));
                s
            }),
    );

    let mut col = column![container(header).padding([4, 8]).width(Fill).style(style::card_header)];

    if !collapsed {
        // The banners share one always-present column, so the code view and the chat keep
        // their positions in the widget tree (and with them, selection, scroll and focus).
        let mut banners = column![];
        if f.annotating {
            let pos = r.claude.pending_for(Target::Annotate(i));
            let msg = match pos {
                Some(0) => format!("✦ Claude is annotating this file… {}", r.claude.activity),
                Some(n) => format!("✦ Annotation queued ({n} request(s) ahead)"),
                None => "✦ Annotating…".into(),
            };
            banners = banners.push(container(text(msg).size(12)).padding([6, 12]).width(Fill).style(style::info_box_style));
        }
        if let Some(e) = &f.annotate_error {
            banners = banners.push(container(style::error_box(e, Some(Message::ClearAnnotations(i)))).padding(6));
        }
        let unanchored = f.threads.iter().filter(|t| t.row(&f.model().rows).is_none()).count();
        if unanchored > 0 {
            banners = banners.push(
                container(
                    row![
                        text(format!("{unanchored} comment thread(s) are on lines outside the shown hunks.")).size(12).width(Fill),
                        button(text("⇕ Full file").size(12)).padding([2, 8]).style(style::flat_button).on_press(Message::ToggleFull(i)),
                    ]
                    .align_y(Alignment::Center),
                )
                .padding([4, 12])
                .width(Fill)
                .style(style::info_box_style),
            );
        }
        if let Some(a) = &f.annotations {
            banners = banners.push(
                container(
                    row![
                        text(format!("✦ {} Claude annotations shown at end of line", a.len())).size(12).width(Fill),
                        button(text("Clear").size(12)).padding([2, 8]).style(style::flat_button).on_press(Message::ClearAnnotations(i)),
                    ]
                    .align_y(Alignment::Center),
                )
                .padding([4, 12])
                .width(Fill)
                .style(style::info_box_style),
            );
        }
        col = col.push(banners);
        if f.data.binary {
            col = col.push(container(text("Binary file not shown.").size(13)).padding(16));
        } else if f.model().rows.is_empty() {
            col = col.push(container(text("No textual changes (mode change or empty file).").size(13)).padding(16));
        } else {
            let model = f.model();
            let full = f.show_full;
            let threads = f.threads.iter().enumerate().filter_map(|(t, th)| {
                Some(Insert {
                    row: th.row(&model.rows)?,
                    badge: thread_badge(r, i, t, th),
                    body: thread_view(r, i, t, th, md),
                    expanded: th.expanded,
                })
            });
            col = col.push(
                container(
                    CodeView::new(&model.rows, &model.spans, colors)
                        .wrap(wrap)
                        .inserts(threads)
                        .annotations(f.annotations.as_ref())
                        .on_context(move |sel| Message::CodeContext(i, full, sel)),
                )
                .padding(Padding { top: 0.0, right: 1.0, bottom: 4.0, left: 1.0 }),
            );
        }
        if let Some(chat) = &f.chat {
            col = col.push(file_chat_view(r, i, f, chat, md));
        }
    }
    container(col).id(file_section_id(i)).width(Fill).style(style::card).into()
}

// ---------------------------------------------------------------------------
// Review comments
// ---------------------------------------------------------------------------

/// A round avatar: the user's picture once it has loaded, their initial until then.
fn avatar<'a>(r: &'a Review, c: &'a ReviewComment, size: f32) -> El<'a> {
    if let Some(h) = c.author.as_ref().and_then(|a| r.avatars.get(&a.avatar_url)) {
        return iced::widget::image(h.clone()).width(size).height(size).border_radius(size / 2.0).into();
    }
    let login = c.login();
    let hue = login.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let palette = [(0x09, 0x69, 0xda), (0x1a, 0x7f, 0x37), (0x82, 0x50, 0xdf), (0xbf, 0x39, 0x89), (0x9a, 0x67, 0x00), (0xcf, 0x22, 0x2e)];
    let (cr, cg, cb) = palette[hue as usize % palette.len()];
    let initial: String = login.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
    container(text(initial).size(size * 0.55).color(Color::WHITE))
        .center_x(size)
        .center_y(size)
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(cr, cg, cb))),
            border: iced::Border { radius: (size / 2.0).into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// The badge at the end of a commented line: the participants' avatars and the
/// number of comments. Clicking it shows or hides the thread.
fn thread_badge<'a>(r: &'a Review, i: usize, t: usize, th: &'a ThreadEntry) -> El<'a> {
    let mut people: Vec<&ReviewComment> = Vec::new();
    for c in &th.thread.comments {
        if !people.iter().any(|p| p.login() == c.login()) {
            people.push(c);
        }
    }
    let avatars = row(people.iter().take(3).map(|c| avatar(r, c, 18.0))).spacing(2);
    let mut content = row![avatars, text(th.thread.comments.len()).size(12)].spacing(4).align_y(Alignment::Center);
    if th.thread.resolved {
        content = content.push(text("✓").size(12).style(|t: &Theme| text::Style { color: Some(style::green(t)) }));
    }
    tooltip(
        button(content)
            .padding([1, 4])
            .style(|t: &Theme, status| {
                let mut s = style::flat_button(t, status);
                if s.background.is_none() {
                    s.background = Some(iced::Background::Color(Color { a: 0.85, ..t.palette().background }));
                }
                s.border.radius = 11.0.into();
                s
            })
            .on_press(Message::ToggleThread(i, t)),
        container(text(if th.expanded { "Hide comments" } else { "Show comments" }).size(12)).padding([4, 8]).style(style::menu),
        tooltip::Position::Left,
    )
    .into()
}

/// GitHub's label for a commenter: "Author" for the PR's author, otherwise their role.
fn commenter_label(pr: &PullRequest, c: &ReviewComment) -> Option<&'static str> {
    if c.login() == pr.author.login {
        return Some("Author");
    }
    match c.author_association.as_str() {
        "OWNER" => Some("Owner"),
        "MEMBER" => Some("Member"),
        "COLLABORATOR" => Some("Collaborator"),
        "CONTRIBUTOR" => Some("Contributor"),
        "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER" => Some("First-time contributor"),
        _ => None,
    }
}

/// A comment thread, shown under the line it is attached to.
fn thread_view<'a>(r: &'a Review, i: usize, t: usize, th: &'a ThreadEntry, md: Md<'a>) -> El<'a> {
    let muted = |t: &Theme| text::Style { color: Some(style::muted(t)) };
    let mut header = row![
        button(
            row![text("⌄").size(14), text(format!("Comment on {}", th.lines_label())).size(13).style(muted)]
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .padding([2, 4])
        .style(style::flat_button)
        .on_press(Message::ToggleThread(i, t)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if th.thread.resolved {
        header = header.push(container(small("Resolved")).padding([1, 8]).style(style::pill(Color::from_rgb8(0x82, 0x50, 0xdf))));
    }
    header = header.push(Space::new().width(Fill));

    let mut comments = column![].spacing(16);
    for (c, body) in th.thread.comments.iter().zip(&th.bodies) {
        let mut name_row = row![
            text(c.login()).size(14).font(fonts::ui_weight(iced::font::Weight::Semibold)),
            text(relative_time(&c.created_at)).size(12).style(muted),
            Space::new().width(Fill),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(label) = commenter_label(&r.pr, c) {
            name_row = name_row.push(container(small(label)).padding([1, 8]).style(|t: &Theme| {
                let mut s = style::pill(style::muted(t))(t);
                s.background = None;
                s.text_color = Some(t.palette().text);
                s
            }));
        }
        comments = comments.push(row![avatar(r, c, 32.0), column![name_row, md_view(body.items(), md)].spacing(6).width(Fill)].spacing(10));
    }

    let reply_url = th.thread.comments.last().map(|c| c.url.clone()).filter(|u| !u.is_empty());
    let footer = row![
        Space::new().width(Fill),
        button(text("Reply on GitHub ↗").size(12)).padding([4, 10]).style(style::default_button).on_press_maybe(reply_url.map(Message::OpenUrl)),
    ];
    container(
        column![
            container(header).padding([6, 10]).width(Fill).style(|t: &Theme| {
                let mut s = style::card_header(t);
                s.border.radius = iced::border::Radius::default().top(6.0);
                s
            }),
            container(comments).padding([12, 16]),
            container(footer).padding(Padding { top: 0.0, right: 12.0, bottom: 10.0, left: 12.0 }),
        ],
    )
    .width(Fill)
    .style(style::card)
    .into()
}

fn chat_key_binding<'a>(send: Message) -> impl Fn(KeyPress) -> Option<Binding<Message>> + 'a {
    move |kp: KeyPress| {
        if let iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter) = kp.key
            && !kp.modifiers.shift() {
                return Some(Binding::Custom(send.clone()));
            }
        Binding::from_key_press(kp)
    }
}

fn chat_entries<'a>(chat: &'a Chat, md: Md<'a>) -> El<'a> {
    column(chat.entries.iter().map(|e| -> El<'a> {
        match e {
            ChatEntry::User(t) => container(
                column![
                    small("You").style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
                    SelectableText::<markdown::Uri, Message>::new([iced::advanced::text::Span::new(t.clone())]).size(14),
                ]
                .spacing(4),
            )
                .padding([8, 10])
                .width(Fill)
                .style(|t| style::bubble(t, true))
                .into(),
            ChatEntry::Assistant { text: raw, md: content, .. } => container(
                column![
                    row![
                        small("✦ Claude").style(|t: &Theme| text::Style { color: Some(style::purple(t)) }),
                        Space::new().width(Fill),
                        button(small(md.copy_label(raw))).padding([0, 6]).style(style::flat_button).on_press(Message::CopyText(raw.clone())),
                    ]
                    .align_y(Alignment::Center),
                    md_view(content.items(), md),
                ]
                .spacing(4),
            )
            .padding([8, 10])
            .width(Fill)
            .style(|t| style::bubble(t, false))
            .into(),
            ChatEntry::Tool(s) => text(format!("⚙ {s}"))
                .size(12)
                .font(fonts::code())
                .style(|t: &Theme| text::Style { color: Some(style::muted(t)) })
                .into(),
            ChatEntry::Error(s) => style::error_box(s, None),
        }
    }))
    .spacing(8)
    .into()
}

fn pending_line<'a>(r: &'a Review, target: Target) -> Option<El<'a>> {
    match r.claude.pending_for(target)? {
        0 => Some(
            row![
                text(format!("✦ Claude is working… {}", r.claude.activity)).size(12).width(Fill),
                button(text("■ Stop").size(12)).padding([2, 8]).style(style::default_button).on_press(Message::StopClaude),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        n => Some(text(format!("Queued — {n} request(s) ahead")).size(12).into()),
    }
}

fn chat_input<'a>(content: &'a text_editor::Content, id: String, on_action: impl Fn(text_editor::Action) -> Message + 'a, send: Message) -> El<'a> {
    row![
        text_editor(content)
            .id(id)
            .placeholder("Ask Claude… (Enter to send, Shift+Enter for a new line)")
            .on_action(on_action)
            .key_binding(chat_key_binding(send.clone()))
            .height(Length::Fixed(72.0))
            .size(14)
            .padding(8),
        primary_btn("Send").on_press(send),
    ]
    .spacing(8)
    .align_y(Alignment::End)
    .into()
}

/// Maximum height of a per-file chat log; it scrolls beyond that.
const FILE_CHAT_LOG_MAX_HEIGHT: f32 = 440.0;

fn file_chat_view<'a>(r: &'a Review, i: usize, f: &'a FileEntry, chat: &'a Chat, md: Md<'a>) -> El<'a> {
    let header = row![
        text(format!("✦ Chat about {}", f.path())).size(13).width(Fill),
        button(text("✕").size(12)).padding([2, 8]).style(style::flat_button).on_press(Message::CloseFileChat(i)),
    ]
    .align_y(Alignment::Center);
    // Everything above the input lives in its own column, so the input keeps the same
    // position in the widget tree (and with it, its focus) as the log and the pending
    // line come and go.
    let mut col = column![header].spacing(8);
    if !chat.entries.is_empty() {
        // Only the log is height-capped. Capping the whole column instead squeezed the
        // input out of view once the log grew past the cap, since a column hands out
        // the remaining height to its children in order.
        col = col.push(
            container(
                scrollable(container(chat_entries(chat, md)).padding(Padding { right: 12.0, ..Padding::ZERO }))
                    .id(file_chat_scroll_id(i))
                    .height(Length::Shrink)
                    .anchor_bottom(),
            )
            .max_height(FILE_CHAT_LOG_MAX_HEIGHT),
        );
    }
    if let Some(p) = pending_line(r, Target::File(i)) {
        col = col.push(p);
    }
    let input = chat_input(&chat.input, file_chat_input_id(i), move |a| Message::FileChatEdit(i, a), Message::FileChatSend(i));
    container(column![col, input].spacing(8)).id(file_chat_id(i)).padding(12).width(Fill).style(style::panel).into()
}

fn general_chat_view<'a>(r: &'a Review, md: Md<'a>) -> El<'a> {
    let chat = &r.general_chat;
    let log: El = if chat.entries.is_empty() {
        container(
            text("Chat with Claude about the PR as a whole. Per-file chats and annotations are available from the ✦ Claude menu on each file.")
                .size(13)
                .style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
        )
        .padding(4)
        .into()
    } else {
        chat_entries(chat, md)
    };
    // The log and the pending line share a column, so the input keeps the same position
    // in the widget tree (and with it, its focus) as the pending line comes and goes.
    let mut log_col = column![
        scrollable(container(log).padding(Padding { right: 12.0, ..Padding::ZERO }))
            .id(GENERAL_CHAT_SCROLL_ID)
            .height(Fill)
            .anchor_bottom(),
    ]
    .spacing(8)
    .height(Fill);
    if let Some(p) = pending_line(r, Target::General) {
        log_col = log_col.push(p);
    }
    let col = column![
        row![text("✦ Claude").size(15).width(Fill), icon_btn("✕", "Hide panel", Message::ToggleChat)].align_y(Alignment::Center),
        log_col,
        chat_input(&chat.input, "general-chat-input".into(), Message::GeneralChatEdit, Message::GeneralChatSend),
    ]
    .spacing(8);
    container(col).padding(12).width(400).height(Fill).style(style::panel).into()
}

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
}

fn build_tree(r: &Review) -> Node {
    let mut root = Node::default();
    for (i, f) in r.files.iter().enumerate() {
        if !r.is_visible(f) {
            continue;
        }
        let mut node = &mut root;
        let parts: Vec<&str> = f.path().split('/').collect();
        for d in &parts[..parts.len() - 1] {
            node = node.dirs.entry((*d).to_string()).or_default();
        }
        node.files.push(i);
    }
    root
}

fn tree_rows<'a>(r: &'a Review, node: &Node, prefix: &str, depth: usize, out: &mut Vec<El<'a>>) {
    let indent = 8.0 + depth as f32 * 14.0;
    for (name, child) in &node.dirs {
        // Compress chains of single directories ("src/highlight").
        let mut label = name.clone();
        let mut full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let mut child = child;
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (n, c) = child.dirs.iter().next().expect("one dir");
            label = format!("{label}/{n}");
            full = format!("{full}/{n}");
            child = c;
        }
        let collapsed = r.collapsed_dirs.contains(&full);
        out.push(
            button(
                row![
                    text(if collapsed { "▸" } else { "▾" }).size(12),
                    text(label).size(13).font(fonts::ui_weight(iced::font::Weight::Semibold)),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .width(Fill)
            .padding(Padding { top: 3.0, bottom: 3.0, left: indent, right: 6.0 })
            .style(style::flat_button)
            .on_press(Message::ToggleDir(full.clone()))
            .into(),
        );
        if !collapsed {
            tree_rows(r, child, &full, depth + 1, out);
        }
    }
    for &i in &node.files {
        let f = &r.files[i];
        let name = f.path().rsplit('/').next().unwrap_or(f.path()).to_string();
        let (icon, kind) = match f.data.file.kind {
            ChangeKind::Added => ("⊞", 0),
            ChangeKind::Deleted => ("⊟", 1),
            ChangeKind::Renamed => ("⇢", 2),
            ChangeKind::Modified => ("±", 3),
        };
        let viewed = f.viewed;
        out.push(
            button(
                row![
                    text(icon).size(13).style(move |t: &Theme| text::Style {
                        color: Some(match kind {
                            0 => style::green(t),
                            1 => style::red(t),
                            2 => style::accent(t),
                            _ => style::muted(t),
                        })
                    }),
                    text(name).size(13).style(move |t: &Theme| text::Style {
                        color: Some(if viewed { Color { a: 0.45, ..t.palette().text } } else { t.palette().text })
                    }),
                    Space::new().width(Fill),
                    text(if viewed { "✓" } else { "" }).size(12).style(|t: &Theme| text::Style { color: Some(style::green(t)) }),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .width(Fill)
            .padding(Padding { top: 3.0, bottom: 3.0, left: indent + 18.0, right: 6.0 })
            .style(style::flat_button)
            .on_press(Message::JumpToFile(i))
            .into(),
        );
    }
}

fn file_tree_view(r: &Review) -> El<'_> {
    // Extension filter chips.
    let mut exts: BTreeMap<String, usize> = BTreeMap::new();
    for f in &r.files {
        *exts.entry(f.extension()).or_default() += 1;
    }
    let chips = row(exts.into_iter().map(|(ext, n)| {
        let on = !r.hidden_exts.contains(&ext);
        button(text(format!("{} {ext} ({n})", if on { "☑" } else { "☐" })).size(12))
            .padding([2, 6])
            .style(if on { style::default_button } else { style::flat_button })
            .on_press(Message::ToggleExt(ext))
            .into()
    }))
    .spacing(4)
    .wrap();

    let mut rows = Vec::new();
    tree_rows(r, &build_tree(r), "", 0, &mut rows);
    container(
        column![
            text_input("Filter files…", &r.filter).on_input(Message::FilterChanged).padding(6).size(13),
            chips,
            scrollable(column(rows).spacing(1)).height(Fill),
        ]
        .spacing(10),
    )
    .padding(10)
    .width(280)
    .height(Fill)
    .style(style::panel)
    .into()
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn status_bar(r: &Review) -> El<'_> {
    let (dot, label) = match &r.claude.status {
        ClaudeStatus::Starting => ("○", "starting…".to_string()),
        ClaudeStatus::Ready => (
            "●",
            if r.claude.in_flight.is_some() {
                let q = r.claude.queue.len();
                format!(
                    "busy — {}{}",
                    if r.claude.activity.is_empty() { "working…" } else { &r.claude.activity },
                    if q > 0 { format!(" ({q} queued)") } else { String::new() }
                )
            } else {
                "idle".into()
            },
        ),
        ClaudeStatus::Exited(_) => ("✕", "exited".into()),
    };
    let ready = r.claude.status == ClaudeStatus::Ready;
    let mut claude_info = format!("Claude {label}");
    if !r.claude.model.is_empty() {
        claude_info.push_str(&format!(" · {}", r.claude.model));
    }
    if !r.claude.permission_mode.is_empty() {
        claude_info.push_str(&format!(" · {}", r.claude.permission_mode));
    }
    if !r.claude.session_id.is_empty() {
        claude_info.push_str(&format!(" · session {}", &r.claude.session_id[..r.claude.session_id.len().min(8)]));
    }
    if r.claude.cost > 0.0 {
        claude_info.push_str(&format!(" · ${:.4}", r.claude.cost));
    }
    let mut bar = row![
        text(dot).size(12).style(move |t: &Theme| text::Style { color: Some(if ready { style::green(t) } else { style::red(t) }) }),
        text(claude_info).size(12),
        button(text("Restart").size(11)).padding([1, 6]).style(style::flat_button).on_press(Message::RestartClaude),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    for (server, st) in &r.lsp {
        let mut s = format!("│  {server}: {}", st.status);
        if let Some((title, msg, pct)) = st.progress.values().next() {
            s = format!("│  {server}: {title}");
            if let Some(p) = pct {
                s.push_str(&format!(" {p}%"));
            }
            if !msg.is_empty() {
                let m: String = msg.chars().take(50).collect();
                s.push_str(&format!(" ({m})"));
            }
        }
        bar = bar.push(text(s).size(12));
        if st.exited.is_some() {
            bar = bar.push(button(text("Restart").size(11)).padding([1, 6]).style(style::flat_button).on_press(Message::StartLsp(*server)));
        } else {
            bar = bar.push(button(text("Stop").size(11)).padding([1, 6]).style(style::flat_button).on_press(Message::StopLsp(*server)));
        }
    }
    if let Some(res) = &r.lsp_last_result {
        bar = bar.push(text(format!("│  {res}")).size(12));
    }
    bar = bar.push(Space::new().width(Fill));
    bar = bar.push(
        text(format!("{} · {}", r.repo.display(), r.branch.as_deref().unwrap_or("?")))
            .size(12)
            .style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
    );
    container(bar).padding([3, 10]).width(Fill).style(style::bar).into()
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

fn context_menu_view<'a>(r: &'a Review, menu: &'a ContextMenu, window: iced::Size) -> El<'a> {
    let word: String = menu.selection.text.trim().lines().next().unwrap_or_default().chars().take(40).collect();
    let path = r.files.get(menu.file).map(|f| f.path()).unwrap_or_default();
    let lsp_note = if r.lsp.iter().any(|(s, st)| s.handles(path) && st.exited.is_none()) {
        String::new()
    } else {
        " (no LSP running)".into()
    };
    let item = |label: String, msg: Message| -> El<'a> {
        button(text(label).size(13)).width(Fill).padding([5, 10]).style(style::flat_button).on_press(msg).into()
    };
    let content = container(
        column![
            container(small(format!("“{word}”")).font(fonts::code())).padding([4, 10]),
            item(format!("Go to definition{lsp_note}"), Message::GotoDefinition),
            item("Copy".into(), Message::CopySelection),
        ]
        .spacing(2),
    )
    .padding(4)
    .width(240)
    .style(style::menu);
    let x = menu.position.x.min(window.width - 250.0).max(0.0);
    let y = menu.position.y.min(window.height - 120.0).max(0.0);
    let positioned = container(content).padding(Padding { top: y, left: x, right: 0.0, bottom: 0.0 });
    stack![
        mouse_area(Space::new().width(Fill).height(Fill))
            .on_press(Message::CloseContextMenu)
            .on_right_press(Message::CloseContextMenu),
        positioned
    ]
    .into()
}

fn permission_view(p: &claude::PermissionRequest, pending: usize) -> El<'_> {
    let input = serde_json::to_string_pretty(&p.input).unwrap_or_default();
    let mut col = column![
        text(format!("Claude wants to use the {} tool", p.tool_name)).size(18),
    ]
    .spacing(10);
    if let Some(d) = &p.description {
        col = col.push(text(d).size(14));
    }
    if let Some(reason) = &p.decision_reason {
        col = col.push(small(format!("Reason for prompt: {reason}")).style(|t: &Theme| text::Style { color: Some(style::muted(t)) }));
    }
    col = col.push(
        container(scrollable(text(input).size(12).font(fonts::code())).height(Length::Shrink))
            .padding(10)
            .max_height(320)
            .width(Fill)
            .style(style::panel),
    );
    let mut buttons = row![Space::new().width(Fill)].spacing(8);
    if pending > 1 {
        buttons = row![small(format!("{} more request(s) waiting", pending - 1)), Space::new().width(Fill)].spacing(8);
    }
    buttons = buttons.push(
        button(text("Deny").size(13)).padding([6, 14]).style(button::danger).on_press(Message::PermissionDecision { allow: false, always: false }),
    );
    if p.suggestions.is_some() {
        buttons = buttons.push(btn("Always allow").on_press(Message::PermissionDecision { allow: true, always: true }));
    }
    buttons = buttons.push(primary_btn("Allow").on_press(Message::PermissionDecision { allow: true, always: false }));
    col = col.push(buttons.align_y(Alignment::Center));
    container(col).padding(20).width(640).style(style::modal).into()
}

fn error_modal_view(err: &str) -> El<'_> {
    container(
        column![
            text("Something went wrong").size(18),
            style::error_box(err, None),
            row![Space::new().width(Fill), primary_btn("OK").on_press(Message::DismissModal)],
        ]
        .spacing(12),
    )
    .padding(20)
    .width(640)
    .style(style::modal)
    .into()
}

fn fonts_section<'a>(ed: &'a SettingsEditor, saved_ui_font: &str) -> El<'a> {
    let label = |s: &'static str| text(s).size(13);
    let help = |s: String| text(s).size(12).style(|t: &Theme| text::Style { color: Some(style::muted(t)) });
    let missing = |name: &str, list: &[String]| -> Option<String> {
        let n = name.trim();
        (!n.is_empty() && !list.iter().any(|f| f == n)).then(|| format!("“{n}” is not installed; the default font will be used."))
    };
    let reset = |msg: Message| button(text("↺").size(13)).padding([3, 8]).style(style::flat_button).on_press(msg);

    let ui_picker = row![
        combo_box(
            &ed.ui_font_options,
            "System default",
            (!ed.ui_font.is_empty()).then_some(&ed.ui_font),
            Message::SettingsUiFont,
        )
        .on_input(Message::SettingsUiFont)
        .size(13)
        .padding(6),
        reset(Message::SettingsUiFont(String::new())),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    let code_picker = row![
        combo_box(
            &ed.code_font_options,
            "System monospace",
            (!ed.code_font.is_empty()).then_some(&ed.code_font),
            Message::SettingsCodeFont,
        )
        .on_input(Message::SettingsCodeFont)
        .size(13)
        .padding(6),
        reset(Message::SettingsCodeFont(String::new())),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let mut ui_col = column![label("UI font"), ui_picker].spacing(4).width(Fill);
    if let Some(m) = missing(&ed.ui_font, &ed.installed_fonts.all) {
        ui_col = ui_col.push(help(m));
    } else if ed.ui_font.trim() != saved_ui_font {
        ui_col = ui_col.push(help("Applied after a restart (Save relaunches the app on the current repository).".into()));
    }
    let mut code_col = column![label("Code font"), code_picker].spacing(4).width(Fill);
    if let Some(m) = missing(&ed.code_font, &ed.installed_fonts.all) {
        code_col = code_col.push(help(m));
    }

    let sliders = row![
        column![
            label("Code font size"),
            row![
                slider(9.0..=22.0, ed.code_font_size, Message::SettingsCodeSize).step(0.5_f32),
                text(format!("{:.1} px", ed.code_font_size)).size(12).width(56),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(4)
        .width(Fill),
        column![
            label("UI zoom"),
            row![
                slider(0.75..=2.0, ed.ui_scale, Message::SettingsUiScale).step(0.05_f32),
                text(format!("{:.0} %", ed.ui_scale * 100.0)).size(12).width(56),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(4)
        .width(Fill),
    ]
    .spacing(16);

    let preview_ui = fonts::family(&ed.ui_font, Font::DEFAULT);
    let preview = container(
        column![
            row![
                text("Bar the frobs, part 2: tidy rendering").size(16).font(Font { weight: iced::font::Weight::Semibold, ..preview_ui }),
                text("#3").size(16).font(preview_ui).style(|t: &Theme| text::Style { color: Some(style::muted(t)) }),
            ]
            .spacing(6),
            text("octocat wants to merge into main · The quick brown fox jumps over the lazy dog.").size(13).font(preview_ui),
            container(
                CodeView::new(&ed.sample.rows, &ed.sample.spans, Colors::new(ed.dark_theme))
                    .font(fonts::family(&ed.code_font, Font::MONOSPACE), ed.code_font_size),
            )
            .style(style::card),
        ]
        .spacing(6),
    )
    .padding(12)
    .width(Fill)
    .style(style::panel);

    column![
        text("Fonts").size(15),
        help("Pick any installed family (type to search; monospace fonts are listed first for code). Code font and sizes apply on Save; UI zoom scales the whole window.".into()),
        row![ui_col, code_col].spacing(16),
        sliders,
        small("Preview"),
        preview,
    ]
    .spacing(8)
    .into()
}

fn settings_view<'a>(ed: &'a SettingsEditor, saved_ui_font: &'a str) -> El<'a> {
    fn editor<'a>(_c: &'static str, content: &'a text_editor::Content, field: SettingsField, h: f32) -> El<'a> {
        text_editor(content)
            .on_action(move |a| Message::SettingsEdit(field, a))
            .height(Length::Fixed(h))
            .size(13)
            .font(fonts::code())
            .padding(8)
            .into()
    }
    let help = |s: &'static str| text(s).size(12).style(|t: &Theme| text::Style { color: Some(style::muted(t)) });
    let mut col = column![
        row![text("Settings").size(20), Space::new().width(Fill), btn("Close").on_press(Message::CloseSettings)].align_y(Alignment::Center),
        row![text("Dark theme").size(15).width(Fill), toggler(ed.dark_theme).on_toggle(Message::SettingsDarkTheme)].align_y(Alignment::Center),
        row![
            column![
                text("Wrap long lines").size(15),
                help("Default for diffs; the ↩ Wrap button on each file overrides it for that file."),
            ]
            .width(Fill),
            toggler(ed.wrap_lines).on_toggle(Message::SettingsWrapLines),
        ]
        .align_y(Alignment::Center),
        fonts_section(ed, saved_ui_font),
        text("Claude command").size(15),
        help("Shell command (run with `sh -c` in the repository directory) that starts Claude Code in stream-json mode. `{repo}` is replaced with the shell-quoted repository path. For Docker, use `docker exec -i` (not `-it`: stdin is a pipe, not a TTY), e.g. docker exec -i -w {repo} dockerclaude claude -p --input-format stream-json …"),
        editor("cmd", &ed.command, SettingsField::Command, 80.0),
        text("Meta prompt: per-file chat").size(15),
        help("Wrapped around messages typed into a file's chat box. Not shown in the chat log. Placeholders: {message} {path} {merge_base} {pr_number} {pr_title} {head_branch} {base_branch}. If {message} is missing, the message is appended."),
        editor("file", &ed.file_chat_meta, SettingsField::FileChatMeta, 130.0),
        text("Meta prompt: general chat").size(15),
        help("Wrapped around messages typed into the side panel chat. Same placeholders ({path} is empty)."),
        editor("general", &ed.general_chat_meta, SettingsField::GeneralChatMeta, 110.0),
        text("Annotate prompt").size(15),
        help("Sent when choosing ✦ Claude → Annotate. The response must contain a JSON object {\"annotations\": {\"<line>\": \"comment\"}} keyed by 1-based line numbers of the new file."),
        editor("annotate", &ed.annotate_prompt, SettingsField::AnnotatePrompt, 170.0),
        small(format!("Saved to {}", crate::settings::Settings::path().display())),
    ]
    .spacing(8);
    if let Some(e) = &ed.error {
        col = col.push(style::error_box(e, None));
    }
    col = col.push(
        row![
            btn("Reset to defaults").on_press(Message::ResetSettings),
            Space::new().width(Fill),
            btn("Cancel").on_press(Message::CloseSettings),
            primary_btn(if ed.ui_font.trim() != saved_ui_font { "Save & restart" } else { "Save" }).on_press(Message::SaveSettings),
        ]
        .spacing(8),
    );
    container(scrollable(container(col).padding(Padding { right: 14.0, ..Padding::ZERO })))
        .padding(20)
        .width(820)
        .max_height(860)
        .style(style::modal)
        .into()
}
