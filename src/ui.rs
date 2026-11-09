use std::path::Path;

use agent_client_protocol::v1::schema::{
    PlanEntryStatus, StopReason, ToolCallContent, ToolCallStatus, ToolKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::App;
use crate::palette;
use crate::session::{Entry, Tool};

const MAX_PLAN_ROWS: usize = 6;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let input = input_view(
        &app.input,
        (area.width as usize).saturating_sub(PROMPT_LEAD.len()),
        (area.height / 2) as usize,
    );
    let plan_rows = if app.show_plan {
        app.transcript.plan.len().min(MAX_PLAN_ROWS)
    } else {
        0
    };
    let [
        header_area,
        transcript_area,
        plan_area,
        upper_rule,
        prompt_area,
        lower_rule,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(plan_rows as u16),
        Constraint::Length(1),
        Constraint::Length(input.height() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(frame, header_area, app);
    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, transcript_area, app);
    draw_plan(frame, plan_area, app);
    draw_rule(frame, upper_rule);
    draw_input(frame, prompt_area, &input);
    draw_rule(frame, lower_rule);
    draw_footer(frame, footer_area, app);
}

fn draw_rule(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(dim()),
        area,
    );
}

pub fn pane(title: &str, focused: bool) -> Block<'static> {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(Padding::horizontal(1));
    if focused {
        let title_style = Style::new().fg(palette::FOCUS).add_modifier(Modifier::BOLD);
        block.title(Span::styled(format!(" {title} "), title_style))
    } else {
        block
            .title(Span::styled(format!(" {title} "), dim()))
            .border_style(dim())
    }
}

/// The two header rows: the coax badge and `title` on the left, `right`
/// against the right edge, and a dim rule below.
pub fn title_bar(frame: &mut Frame, area: Rect, title: &str, right: Line) {
    let [top, rule] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let badge = Style::new()
        .fg(palette::INK)
        .bg(palette::REF)
        .add_modifier(Modifier::BOLD);
    let left = Line::from(vec![
        Span::styled(" coax ", badge),
        Span::raw("  "),
        Span::styled(title.to_string(), bold()),
    ]);
    let gap = (area.width as usize)
        .saturating_sub(left.width() + right.width())
        .max(1);
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right.spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), top);
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(dim()),
        rule,
    );
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let state = if app.transcript.in_flight {
        Span::styled("◐ working", Style::new().fg(palette::WORKING))
    } else {
        match &app.transcript.last_stop {
            Some(stop) if *stop != StopReason::EndTurn => {
                Span::styled(format!("idle, {stop:?}"), Style::new().fg(palette::WORKING))
            }
            _ => Span::styled("idle", dim()),
        }
    };
    let mut right = vec![state];
    match &app.decade_pane {
        Some(pane) if pane.is_empty() => right.push(Span::styled("   decade", dim())),
        Some(pane) => right.push(Span::styled(format!("   decade pane {pane}"), dim())),
        None => {}
    }
    let session: String = app.session.to_string().chars().take(8).collect();
    right.push(Span::styled(format!("   session {session} "), dim()));
    title_bar(frame, area, &app.agent, Line::from(right));
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let plan = &app.transcript.plan;
    let mut hints = vec![if app.permission.is_some() {
        "1-9 choose"
    } else if app.transcript.in_flight {
        "esc cancel"
    } else {
        "enter send"
    }];
    if app.permission.is_none() {
        hints.push("pgup/pgdn scroll");
        if !plan.is_empty() {
            hints.push("^t plan");
        }
    } else {
        hints.push("esc cancel");
    }
    hints.push("^c quit");
    let left = format!("  {}", hints.join("   "));
    let right = if plan.is_empty() {
        String::new()
    } else {
        let done = plan
            .iter()
            .filter(|entry| entry.status == PlanEntryStatus::Completed)
            .count();
        format!("plan {done}/{} done ", plan.len())
    };
    let gap = (area.width as usize)
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(left),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, dim()),
        ])),
        area,
    );
}

fn draw_plan(frame: &mut Frame, area: Rect, app: &App) {
    let room = (area.width as usize).saturating_sub(2);
    let lines: Vec<Line> = app
        .transcript
        .plan
        .iter()
        .take(area.height as usize)
        .map(|entry| {
            let (glyph, style) = match entry.status {
                PlanEntryStatus::InProgress => ("◐", Style::new().fg(palette::WORKING)),
                PlanEntryStatus::Completed => ("●", Style::new().fg(palette::DONE)),
                _ => ("○", dim()),
            };
            Line::from(vec![
                Span::styled(format!("{glyph} "), style),
                Span::raw(cut(&entry.content, room)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Agent text laid out as markdown, kept per transcript entry.
#[derive(Default)]
pub struct Rendered {
    entries: Vec<Option<Markdown>>,
}

struct Markdown {
    width: usize,

    // agent text only grows by appending, so an
    // unchanged length means unchanged text
    len: usize,
    lines: Vec<Line<'static>>,
}

impl Rendered {
    fn agent(&mut self, index: usize, text: &str, width: usize) -> &[Line<'static>] {
        if self.entries.len() <= index {
            self.entries.resize_with(index + 1, || None);
        }
        let slot = &mut self.entries[index];
        let fresh = matches!(slot, Some(done) if done.width == width && done.len == text.len());
        if !fresh {
            *slot = Some(Markdown {
                width,
                len: text.len(),
                lines: markdown(text, width),
            });
        }
        slot.as_ref().map_or(&[], |done| &done.lines)
    }
}

fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    cmr::render(text, width)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(text, style)| Span::styled(text, screen_style(style)))
                .collect()
        })
        .collect()
}

fn screen_style(style: cmr::Style) -> Style {
    let mut out = Style::new();
    if let Some(color) = style.fg {
        out = out.fg(match color {
            cmr::Color::Black => Color::Black,
            cmr::Color::Red => Color::Red,
            cmr::Color::Green => Color::Green,
            cmr::Color::Yellow => Color::Yellow,
            cmr::Color::Blue => Color::Blue,
            cmr::Color::Magenta => Color::Magenta,
            cmr::Color::Cyan => Color::Cyan,
            cmr::Color::Gray => Color::Gray,
            cmr::Color::DarkGray => Color::DarkGray,
            cmr::Color::LightRed => Color::LightRed,
            cmr::Color::LightGreen => Color::LightGreen,
            cmr::Color::LightYellow => Color::LightYellow,
            cmr::Color::LightBlue => Color::LightBlue,
            cmr::Color::LightMagenta => Color::LightMagenta,
            cmr::Color::LightCyan => Color::LightCyan,
            cmr::Color::White => Color::White,
        });
    }
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        out = out.add_modifier(Modifier::DIM);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let width = area.width.max(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (index, entry) in app.transcript.entries.iter().enumerate() {
        match entry {
            Entry::User(text) => {
                let first = lines.len();
                wrapped(
                    "> ",
                    "  ",
                    bold(),
                    text,
                    width,
                    Style::default(),
                    &mut lines,
                );
                for line in &mut lines[first..] {
                    let pad = width.saturating_sub(line.width());
                    line.push_span(" ".repeat(pad));
                    line.style = Style::new().bg(palette::SHADE);
                }
            }
            Entry::Agent(text) => {
                let body = app
                    .rendered
                    .agent(index, text, width.saturating_sub(2).max(1));
                let lead = Span::styled("● ", Style::new().fg(palette::AGENT));
                if body.is_empty() {
                    lines.push(Line::from(lead.clone()));
                }
                for (n, row) in body.iter().enumerate() {
                    let start = if n == 0 {
                        lead.clone()
                    } else {
                        Span::raw("  ")
                    };
                    let mut spans = vec![start];
                    spans.extend(row.spans.iter().cloned());
                    lines.push(Line::from(spans));
                }
            }
            Entry::Thought(text) => wrapped("✻ ", "  ", dim(), text, width, dim(), &mut lines),
            Entry::Tool(tool) => lines.extend(tool_block(tool, &app.cwd, width)),
            Entry::Permission { title, choice } => {
                lines.push(Line::from(format!("  ⎿  {choice}  {title}")).style(dim()));
            }
            Entry::Notice(text) => {
                lines
                    .push(Line::from(format!("! {text}")).style(Style::new().fg(palette::WORKING)));
            }
        }
        lines.push(Line::default());
    }
    let height = area.height as usize;
    let rows = height.saturating_sub(1).max(1);
    app.page = rows;
    if app.scroll > 0 {
        app.scroll += lines.len().saturating_sub(app.lines_seen);
    }
    app.lines_seen = lines.len();
    app.scroll = app.scroll.min(lines.len().saturating_sub(rows));
    let rows = if app.scroll > 0 { rows } else { height };
    let end = lines.len() - app.scroll;
    let mut visible: Vec<Line> = lines
        .into_iter()
        .take(end)
        .skip(end.saturating_sub(rows))
        .collect();
    if app.scroll > 0 {
        visible.push(Line::styled(format!("⋯ {} more below", app.scroll), dim()));
    }
    frame.render_widget(Paragraph::new(visible), area);
}

fn wrapped(
    first: &str,
    rest: &str,
    lead_style: Style,
    text: &str,
    width: usize,
    style: Style,
    out: &mut Vec<Line>,
) {
    let inner = width.saturating_sub(first.chars().count()).max(1);
    let mut lead = first;
    for paragraph in text.split('\n') {
        let pieces = textwrap::wrap(paragraph, inner);
        if pieces.is_empty() {
            out.push(Line::from(Span::styled(lead.to_string(), lead_style)));
            lead = rest;
        }
        for piece in pieces {
            out.push(Line::from(vec![
                Span::styled(lead.to_string(), lead_style),
                Span::styled(piece.into_owned(), style),
            ]));
            lead = rest;
        }
    }
}

fn tool_block(tool: &Tool, cwd: &Path, width: usize) -> [Line<'static>; 2] {
    let (dot, status, status_style) = match tool.status {
        ToolCallStatus::InProgress => (
            Style::new().fg(palette::WORKING),
            "running…",
            Style::new().fg(palette::WORKING),
        ),
        ToolCallStatus::Completed => (Style::new().fg(palette::DONE), "done", dim()),
        ToolCallStatus::Failed => (
            Style::new().fg(palette::ALARM),
            "failed",
            Style::new().fg(palette::ALARM),
        ),
        _ => (dim(), "pending", dim()),
    };
    let head = [
        Span::styled("● ", dot),
        Span::styled(format!("{:<6}", kind_word(tool.kind)), bold()),
        Span::raw(" "),
    ];
    let room = width.saturating_sub(head.iter().map(Span::width).sum());
    let mut first = head.to_vec();
    first.push(Span::raw(cut(&tool.title, room)));

    let lead = Span::styled("  ⎿  ", dim());
    let room = width.saturating_sub(lead.width());
    let mut second = vec![lead];
    let mut detail = Vec::new();
    let (added, removed) = diff_size(&tool.content);
    let mut used = 0;
    if added + removed > 0 {
        let plus = format!("+{added}");
        let minus = format!(" −{removed}");
        used = plus.chars().count() + minus.chars().count();
        detail.push(Span::styled(plus, Style::new().fg(palette::DONE)));
        detail.push(Span::styled(minus, Style::new().fg(palette::ALARM)));
    }

    // the location gives way when the row cannot hold it
    let place = location(tool, cwd).filter(|place| place.chars().count() + 2 + used <= room);
    if let Some(place) = place {
        second.push(Span::styled(place, dim()));
        if used > 0 {
            second.push(Span::raw("  "));
        }
    }
    second.extend(detail);
    if second.len() == 1 {
        second.push(Span::styled(status, status_style));
    }
    [Line::from(first), Line::from(second)]
}

fn kind_word(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "run",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "mode",
        _ => "tool",
    }
}

fn location(tool: &Tool, cwd: &Path) -> Option<String> {
    let location = tool.locations.first()?;
    let path = location.path.strip_prefix(cwd).unwrap_or(&location.path);
    let path = path.display().to_string();
    if tool.title.contains(&path) {
        return None;
    }
    Some(match location.line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    })
}

fn diff_size(content: &[ToolCallContent]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for content in content {
        if let ToolCallContent::Diff(diff) = content {
            added += diff.new_text.lines().count();
            removed += diff
                .old_text
                .as_ref()
                .map_or(0, |text| text.lines().count());
        }
    }
    (added, removed)
}

pub fn cut(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(room - 1).collect();
    out.push('…');
    out
}

fn draw_permission(frame: &mut Frame, over: Rect, app: &App) {
    let Some(permission) = &app.permission else {
        return;
    };
    let mut lines = Vec::new();
    let content = permission.update.fields.content.as_deref().unwrap_or(&[]);
    let (added, removed) = diff_size(content);
    if added + removed > 0 {
        lines.push(Line::from(vec![
            Span::styled(format!("+{added}"), Style::new().fg(palette::DONE)),
            Span::styled(format!(" −{removed}"), Style::new().fg(palette::ALARM)),
        ]));
        lines.push(Line::default());
    }
    for (n, option) in permission.options.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", n + 1), focus().add_modifier(Modifier::BOLD)),
            Span::raw(option.name.clone()),
        ]));
    }
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let wanted = content_width.max(permission.title.chars().count() + 2) + 4;
    let width = (wanted as u16).clamp(24.min(over.width), over.width);
    let height = (lines.len() as u16 + 2).min(over.height);
    let area = Rect {
        x: over.x + (over.width - width) / 2,
        y: over.y + (over.height - height) / 2,
        width,
        height,
    };
    let title = cut(&permission.title, (width as usize).saturating_sub(4));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(pane(&title, true)), area);
}

fn draw_input(frame: &mut Frame, area: Rect, input: &InputView) {
    let lines: Vec<Line> = input
        .rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let lead = if n == 0 && input.starts_here {
                Span::styled(PROMPT_FIRST, bold())
            } else {
                Span::raw(PROMPT_LEAD)
            };
            Line::from(vec![lead, Span::raw(row.clone())])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    let (column, row) = input.cursor;
    frame.set_cursor_position((area.x + column, area.y + row));
}

/// The prompt rows that fit on screen and where the cursor sits among them.
struct InputView {
    rows: Vec<String>,

    /// Whether `rows` begins at the start of the text, so the first row
    /// carries the `> ` prefix.
    starts_here: bool,

    /// Column and row of the cursor within the prompt rows, prefix included.
    cursor: (u16, u16),
}

const PROMPT_FIRST: &str = "> ";
const PROMPT_LEAD: &str = "  ";
const PROMPT_ROWS: usize = 3;

impl InputView {
    fn height(&self) -> usize {
        self.rows.len().max(PROMPT_ROWS)
    }
}

fn input_view(text: &str, width: usize, max_rows: usize) -> InputView {
    let all = wrap_input(text, width);
    let shown = max_rows.max(PROMPT_ROWS);
    let skipped = all.len().saturating_sub(shown);
    let rows = all[skipped..].to_vec();
    let last = rows.last().map_or(0, |row| row.chars().count());
    let cursor = (
        (PROMPT_LEAD.len() + last) as u16,
        rows.len().saturating_sub(1) as u16,
    );
    InputView {
        rows,
        starts_here: skipped == 0,
        cursor,
    }
}

/// Break `text` into rows of at most `width` characters at word breaks.
/// A word longer than a row is split, and the last row is never full, so the
/// cursor always has a column after the last character.
fn wrap_input(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut rows = vec![String::new()];
    let mut used = 0;
    for token in chars.chunk_by(|a, b| (*a == ' ') == (*b == ' ')) {
        let is_space = token[0] == ' ';
        if used + token.len() > width && !is_space && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        for &ch in token {
            if used == width {
                rows.push(String::new());
                used = 0;
            }
            rows.last_mut().unwrap().push(ch);
            used += 1;
        }
    }
    if used == width {
        rows.push(String::new());
    }
    rows
}

pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn focus() -> Style {
    Style::new().fg(palette::FOCUS)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{Diff, ToolCallLocation};

    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect()
    }

    #[test]
    fn agent_text_is_laid_out_again_when_it_grows_or_the_width_changes() {
        let mut rendered = Rendered::default();
        assert_eq!(plain(rendered.agent(0, "one two", 20)), ["one two"]);
        assert_eq!(
            plain(rendered.agent(0, "one two three", 20)),
            ["one two three"]
        );
        assert_eq!(
            plain(rendered.agent(0, "one two three", 8)),
            ["one two", "three"]
        );
    }

    fn tool(title: &str, path: &str, line: Option<u32>) -> Tool {
        let mut location = ToolCallLocation::new(path);
        location.line = line;
        Tool {
            id: "c1".into(),
            title: title.to_string(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            locations: vec![location],
            content: Vec::new(),
        }
    }

    fn rows(tool: &Tool, width: usize) -> [String; 2] {
        tool_block(tool, Path::new("/work"), width).map(|line| line.to_string())
    }

    #[test]
    fn a_tool_title_too_long_for_the_row_is_cut_with_an_ellipsis() {
        let tool = tool("Read a/very/long/path/to/some/file.rs", "/work/x.rs", None);
        let [first, second] = rows(&tool, 30);
        assert_eq!(first, "● read   Read a/very/long/pat…");
        assert_eq!(first.chars().count(), 30);
        assert_eq!(second, "  ⎿  x.rs");
    }

    #[test]
    fn the_location_is_dropped_when_the_title_already_names_the_file() {
        let shown = tool("Read src/main.rs", "/work/src/main.rs", Some(3));
        assert_eq!(rows(&shown, 80)[1], "  ⎿  done");
        let other = tool("Read the entry point", "/work/src/main.rs", Some(3));
        assert_eq!(rows(&other, 80)[1], "  ⎿  src/main.rs:3");
    }

    #[test]
    fn the_location_gives_way_when_the_row_cannot_hold_it() {
        let tool = tool("Read the entry point", "/work/src/main.rs", Some(3));
        let [first, second] = rows(&tool, 15);
        assert_eq!(first, "● read   Read …");
        assert_eq!(second, "  ⎿  done");
    }

    #[test]
    fn an_edit_shows_its_size_after_the_location() {
        let mut edit = tool("Write the notes", "/work/NOTES.md", None);
        edit.content = vec![ToolCallContent::Diff(Diff::new("NOTES.md", "a\nb\n"))];
        assert_eq!(rows(&edit, 80)[1], "  ⎿  NOTES.md  +2 −0");
    }

    #[test]
    fn a_tool_with_nothing_to_show_names_its_status() {
        let mut pending = tool("Read src/main.rs", "/work/src/main.rs", None);
        pending.status = ToolCallStatus::InProgress;
        assert_eq!(rows(&pending, 80)[1], "  ⎿  running…");
        pending.status = ToolCallStatus::Failed;
        assert_eq!(rows(&pending, 80)[1], "  ⎿  failed");
    }

    #[test]
    fn agent_markdown_keeps_its_styles_on_screen() {
        let lines = markdown("# Title\n\nsome `code`", 40);
        let title = &lines[0].spans[0];
        assert!(title.style.add_modifier.contains(Modifier::BOLD));
        let code = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "code")
            .unwrap();
        assert_eq!(code.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn input_wraps_at_word_breaks() {
        assert_eq!(
            wrap_input("one two three four", 9),
            ["one two ", "three ", "four"]
        );
    }

    #[test]
    fn a_word_longer_than_the_row_is_split() {
        assert_eq!(wrap_input("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn the_cursor_follows_trailing_spaces() {
        let view = input_view("hi   ", 20, 10);
        assert_eq!(view.cursor, (2 + 5, 0));
    }

    #[test]
    fn a_row_filled_to_the_edge_puts_the_cursor_on_a_new_row() {
        let view = input_view("abcd", 4, 10);
        assert_eq!(view.rows, ["abcd", ""]);
        assert_eq!(view.cursor, (2, 1));
    }

    #[test]
    fn the_prompt_shows_three_rows_for_short_text_and_grows_with_long_text() {
        assert_eq!(input_view("", 20, 10).height(), 3);
        let long = "word ".repeat(12);
        assert_eq!(input_view(&long, 20, 10).height(), 4);
    }

    #[test]
    fn past_the_limit_the_prompt_shows_the_last_rows() {
        let text = "a ".repeat(30);
        let view = input_view(&text, 4, 5);
        assert_eq!(view.rows.len(), 5);
        assert!(!view.starts_here);
        assert_eq!(view.cursor.1, 4);
    }
}
