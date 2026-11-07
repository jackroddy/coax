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
    let [plan_area, transcript_area, status_area, input_area] = Layout::vertical([
        Constraint::Length(plan_rows),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_plan(frame, plan_area, app);
    draw_transcript(frame, transcript_area, app);
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

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let width = area.width.max(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript.entries {
        entry_lines(entry, width, &mut lines);
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
        Entry::Agent(text) => wrapped("", text, width, Style::default(), out),
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

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let state = if app.transcript.in_flight {
        "working".to_string()
    } else {
        match &app.transcript.last_stop {
            Some(stop) => format!("idle, last stop {stop:?}"),
            None => "idle".to_string(),
        }
    };
    let text = format!(" {}  session {}  {state}", app.agent, app.session);
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
