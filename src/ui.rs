use agent_client_protocol::v1::schema::{
    PlanEntryStatus, ToolCallContent, ToolCallStatus, ToolKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::session::{Entry, Tool};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let plan_rows = app.transcript.plan.len().min(6) as u16;
    let permission_rows = if app.permission.is_some() { 2 } else { 0 };
    let [
        plan_area,
        transcript_area,
        permission_area,
        status_area,
        input_area,
    ] = Layout::vertical([
        Constraint::Length(plan_rows),
        Constraint::Min(1),
        Constraint::Length(permission_rows),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_plan(frame, plan_area, app);
    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, permission_area, app);
    draw_status(frame, status_area, app);
    draw_input(frame, input_area, app);
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
    for (index, entry) in app.transcript.entries.iter().enumerate() {
        match entry {
            Entry::Agent(text) => lines.extend_from_slice(app.rendered.agent(index, text, width)),
            entry => entry_lines(entry, width, &mut lines),
        }
        lines.push(Line::default());
    }
    let height = area.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let start = max_scroll - app.scroll;
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn entry_lines(entry: &Entry, width: usize, out: &mut Vec<Line>) {
    match entry {
        Entry::User(text) => wrapped("> ", text, width, bold(), out),
        Entry::Agent(text) => out.extend(markdown(text, width)),
        Entry::Thought(text) => wrapped("~ ", text, width, dim(), out),
        Entry::Tool(tool) => tool_lines(tool, out),
        Entry::Permission { title, choice } => {
            out.push(Line::from(format!("? {title} -> {choice}")).style(dim()));
        }
        Entry::Notice(text) => {
            out.push(Line::from(format!("! {text}")).style(Style::default().fg(Color::Yellow)));
        }
    }
}

fn wrapped(prefix: &str, text: &str, width: usize, style: Style, out: &mut Vec<Line>) {
    let inner = width.saturating_sub(prefix.len()).max(1);
    let indent = " ".repeat(prefix.len());
    for (i, paragraph) in text.split('\n').enumerate() {
        let pieces = textwrap::wrap(paragraph, inner);
        if pieces.is_empty() {
            out.push(Line::default());
        }
        for (j, piece) in pieces.iter().enumerate() {
            let lead = if i == 0 && j == 0 { prefix } else { &indent };
            out.push(Line::from(format!("{lead}{piece}")).style(style));
        }
    }
}

fn tool_lines(tool: &Tool, out: &mut Vec<Line>) {
    let kind = match tool.kind {
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
    };
    let (status, color) = match tool.status {
        ToolCallStatus::Pending => ("pending", Color::DarkGray),
        ToolCallStatus::InProgress => ("running", Color::Cyan),
        ToolCallStatus::Completed => ("done", Color::Green),
        ToolCallStatus::Failed => ("failed", Color::Red),
        _ => ("unknown", Color::DarkGray),
    };
    out.push(Line::from(vec![
        Span::styled(format!("* {kind:<6} "), dim()),
        Span::raw(tool.title.clone()),
        Span::styled(format!("  {status}"), Style::default().fg(color)),
    ]));
    for location in &tool.locations {
        let place = match location.line {
            Some(line) => format!("    {}:{line}", location.path.display()),
            None => format!("    {}", location.path.display()),
        };
        out.push(Line::from(place).style(dim()));
    }
    for content in &tool.content {
        if let ToolCallContent::Diff(diff) = content {
            let added = diff.new_text.lines().count();
            let removed = diff
                .old_text
                .as_ref()
                .map(|text| text.lines().count())
                .unwrap_or(0);
            let summary = format!("    {} +{added} -{removed}", diff.path.display());
            out.push(Line::from(summary).style(dim()));
        }
    }
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

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let state = if app.transcript.in_flight {
        "working".to_string()
    } else {
        match &app.transcript.last_stop {
            Some(stop) => format!("idle, last stop {stop:?}"),
            None => "idle".to_string(),
        }
    };
    let decade = match &app.decade_pane {
        Some(pane) if pane.is_empty() => "  decade".to_string(),
        Some(pane) => format!("  decade pane {pane}"),
        None => String::new(),
    };
    let text = format!(" {}  session {}  {state}{decade}", app.agent, app.session);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let text = format!("> {}", app.input);
    frame.render_widget(Paragraph::new(text.clone()), area);
    let column = area.x + (text.chars().count() as u16).min(area.width.saturating_sub(1));
    frame.set_cursor_position((column, area.y));
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
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
