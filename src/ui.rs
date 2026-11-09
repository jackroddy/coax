use std::path::Path;

use agent_client_protocol::v1::schema::{
    PlanEntryStatus, StopReason, ToolCallContent, ToolCallStatus, ToolKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use crate::app::App;
use crate::palette;
use crate::session::{Entry, Tool};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let plan_rows = app.transcript.plan.len().min(6) as u16;
    let permission_rows = if app.permission.is_some() { 2 } else { 0 };
    let [
        header_area,
        plan_area,
        session_area,
        permission_area,
        prompt_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(plan_rows),
        Constraint::Min(3),
        Constraint::Length(permission_rows),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header_area, app);
    draw_plan(frame, plan_area, app);
    let session = pane("session", false);
    let transcript_area = session.inner(session_area);
    frame.render_widget(session, session_area);
    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, permission_area, app);
    draw_input(frame, prompt_area, app);
    draw_footer(frame, footer_area, app);
}

fn pane(title: &str, focused: bool) -> Block<'static> {
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

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let [top, rule] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let badge = Style::new()
        .fg(palette::INK)
        .bg(palette::REF)
        .add_modifier(Modifier::BOLD);
    let left = Line::from(vec![
        Span::styled(" coax ", badge),
        Span::raw("  "),
        Span::styled(app.agent.clone(), bold()),
    ]);

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
    let right = Line::from(right);

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

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hints = if app.permission.is_some() {
        "1-9 choose   esc cancel   ^c quit"
    } else if app.transcript.in_flight {
        "esc cancel   pgup/pgdn scroll   ^c quit"
    } else {
        "enter send   pgup/pgdn scroll   ^u clear   ^c quit"
    };
    frame.render_widget(Paragraph::new(hints), area);
}

fn draw_plan(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let lines: Vec<Line> = app
        .transcript
        .plan
        .iter()
        .take(area.height as usize)
        .map(|entry| {
            let mark = match entry.status {
                PlanEntryStatus::Pending => "[ ]",
                PlanEntryStatus::InProgress => "[>]",
                PlanEntryStatus::Completed => "[x]",
                _ => "[?]",
            };
            Line::from(format!("{mark} {}", entry.content)).style(dim())
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
    let entries = &app.transcript.entries;
    let mut index = 0;
    while index < entries.len() {
        match &entries[index] {
            Entry::User(text) => wrapped("› ", "  ", focus(), text, width, bold(), &mut lines),
            Entry::Agent(text) => lines.extend_from_slice(app.rendered.agent(index, text, width)),
            Entry::Thought(text) => wrapped("│ ", "│ ", dim(), text, width, dim(), &mut lines),
            Entry::Tool(_) => {
                let run: Vec<&Tool> = entries[index..]
                    .iter()
                    .map_while(|entry| match entry {
                        Entry::Tool(tool) => Some(tool),
                        _ => None,
                    })
                    .collect();
                tool_run(&run, &app.cwd, width, &mut lines);
                index += run.len() - 1;
            }
            Entry::Permission { title, choice } => {
                lines.push(Line::from(format!("? {title} -> {choice}")).style(dim()));
            }
            Entry::Notice(text) => {
                lines
                    .push(Line::from(format!("! {text}")).style(Style::new().fg(palette::WORKING)));
            }
        }
        lines.push(Line::default());
        index += 1;
    }
    let height = area.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let start = max_scroll - app.scroll;
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
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

fn tool_run(tools: &[&Tool], cwd: &Path, width: usize, out: &mut Vec<Line>) {
    let noun = if tools.len() == 1 { "tool" } else { "tools" };
    out.push(Line::from(vec![
        Span::styled("✦ ", Style::new().fg(palette::AGENT)),
        Span::styled(format!("{} {noun}", tools.len()), dim()),
    ]));
    for (n, tool) in tools.iter().enumerate() {
        let branch = if n + 1 == tools.len() {
            "└─ "
        } else {
            "├─ "
        };
        out.push(tool_line(tool, branch, cwd, width));
    }
}

fn tool_line(tool: &Tool, branch: &str, cwd: &Path, width: usize) -> Line<'static> {
    let (glyph, glyph_style) = match tool.status {
        ToolCallStatus::InProgress => ("◐", Style::new().fg(palette::WORKING)),
        ToolCallStatus::Completed => ("●", Style::new().fg(palette::DONE)),
        ToolCallStatus::Failed => ("●", Style::new().fg(palette::ALARM)),
        _ => ("○", dim()),
    };
    let head = vec![
        Span::styled(format!("  {branch}"), dim()),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(format!("{:<6} ", kind_word(tool.kind)), dim()),
    ];
    let room = width.saturating_sub(head.iter().map(Span::width).sum());

    let mut diff = Vec::new();
    let (added, removed) = diff_size(tool);
    if added + removed > 0 {
        diff.push(Span::styled(
            format!("  +{added}"),
            Style::new().fg(palette::DONE),
        ));
        diff.push(Span::styled(
            format!(" −{removed}"),
            Style::new().fg(palette::ALARM),
        ));
    }
    let diff_width: usize = diff.iter().map(Span::width).sum();
    let title_width = tool.title.chars().count();

    // the title keeps its room before the location does
    let place = location(tool, cwd)
        .map(|place| format!("  {place}"))
        .filter(|place| title_width + place.chars().count() + diff_width <= room);
    let place_width = place.as_ref().map_or(0, |place| place.chars().count());
    let title = cut(&tool.title, room.saturating_sub(place_width + diff_width));

    let mut spans = head;
    spans.push(Span::raw(title));
    if let Some(place) = place {
        spans.push(Span::styled(place, dim()));
    }
    spans.extend(diff);
    Line::from(spans)
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

fn diff_size(tool: &Tool) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for content in &tool.content {
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

fn cut(text: &str, room: usize) -> String {
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

fn draw_permission(frame: &mut Frame, area: Rect, app: &App) {
    let Some(permission) = &app.permission else {
        return;
    };
    let options: Vec<String> = permission
        .options
        .iter()
        .enumerate()
        .map(|(i, option)| format!("[{}] {}", i + 1, option.name))
        .collect();
    let lines = vec![
        Line::from(format!("? {}", permission.title)).style(Style::default().fg(Color::Yellow)),
        Line::from(format!("  {}   [Esc] cancel", options.join("  "))),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let block = pane("prompt", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(app.input.as_str()), inner);
    let typed = app.input.chars().count() as u16;
    let column = inner.x + typed.min(inner.width.saturating_sub(1));
    frame.set_cursor_position((column, inner.y));
}

fn dim() -> Style {
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
    use agent_client_protocol::v1::schema::ToolCallLocation;

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

    #[test]
    fn a_tool_title_too_long_for_the_pane_is_cut_with_an_ellipsis() {
        let tool = tool("Read a/very/long/path/to/some/file.rs", "/work/x.rs", None);
        let line = tool_line(&tool, "└─ ", Path::new("/work"), 30).to_string();
        assert_eq!(line, "  └─ ● read   Read a/very/lon…");
        assert_eq!(line.chars().count(), 30);
    }

    #[test]
    fn the_location_is_dropped_when_the_title_already_names_the_file() {
        let shown = tool("Read src/main.rs", "/work/src/main.rs", Some(3));
        let line = tool_line(&shown, "└─ ", Path::new("/work"), 80).to_string();
        assert_eq!(line, "  └─ ● read   Read src/main.rs");
        let other = tool("Read the entry point", "/work/src/main.rs", Some(3));
        let line = tool_line(&other, "└─ ", Path::new("/work"), 80).to_string();
        assert_eq!(line, "  └─ ● read   Read the entry point  src/main.rs:3");
    }

    #[test]
    fn the_location_gives_way_before_the_title_is_cut() {
        let tool = tool("Read the entry point", "/work/src/main.rs", Some(3));
        let line = tool_line(&tool, "└─ ", Path::new("/work"), 40).to_string();
        assert_eq!(line, "  └─ ● read   Read the entry point");
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
}
