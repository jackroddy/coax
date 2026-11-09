use std::path::Path;

use agent_client_protocol::v1::schema::{
    PlanEntryStatus, ToolCallContent, ToolCallId, ToolCallStatus, ToolKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::{App, ExitKey, MenuTarget};
use crate::command;
use crate::header;
use crate::palette;
use crate::session::{Child, Entry, Tool};
use crate::settings::{self, Control};

const MAX_PLAN_ROWS: usize = 6;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let foreground = Style::new().fg(palette::get().foreground);
    frame.buffer_mut().set_style(area, foreground);
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
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(plan_rows as u16),
        Constraint::Length(1),
        Constraint::Length(input.height() as u16),
        Constraint::Length(1),
    ])
    .areas(area);

    header::draw_header(frame, header_area, app);
    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, transcript_area, app);
    draw_menu(frame, app);
    draw_plan(frame, plan_area, app);
    draw_rule(frame, upper_rule);
    draw_input(frame, prompt_area, &input, prompt_colour(&app.input));
    draw_commands(frame, app, upper_rule);
    draw_confirm(frame, transcript_area, app);
    draw_guard_rule(frame, lower_rule, app);
}

/// The rule under the prompt, carrying the exit guard's message while armed.
fn draw_guard_rule(frame: &mut Frame, area: Rect, app: &App) {
    let Some(guard) = app.guard else {
        return draw_rule(frame, area);
    };
    let message = match guard.key {
        ExitKey::CtrlC => "press ^c again to quit",
        ExitKey::CtrlD => "press ^d again to quit",
    };
    let rest = (area.width as usize).saturating_sub(message.chars().count() + 4);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("── ", dim()),
            Span::styled(message, Style::new().fg(palette::get().yellow)),
            Span::styled(format!(" {}", "─".repeat(rest)), dim()),
        ])),
        area,
    );
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
        let title_style = Style::new()
            .fg(palette::get().blue)
            .add_modifier(Modifier::BOLD);
        block.title(Span::styled(format!(" {title} "), title_style))
    } else {
        block
            .title(Span::styled(format!(" {title} "), dim()))
            .border_style(dim())
    }
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
                PlanEntryStatus::InProgress => ("◐", Style::new().fg(palette::get().yellow)),
                PlanEntryStatus::Completed => ("●", Style::new().fg(palette::get().green)),
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
    parts: Vec<Part>,
}

/// A run of agent text: rows that fit the layout width, or one code block or
/// table wider than it.
enum Part {
    Flow(Vec<Line<'static>>),
    Wide {
        /// Which block of the entry this is.
        block: usize,

        /// The rows at the block's natural width, trailing spaces trimmed.
        rows: Vec<Line<'static>>,
        natural: usize,
    },
}

/// The widest a table is laid out when measuring it.
const TABLE_WIDTH: usize = 1000;

impl Rendered {
    fn agent(&mut self, index: usize, text: &str, width: usize) -> &[Part] {
        if self.entries.len() <= index {
            self.entries.resize_with(index + 1, || None);
        }
        let slot = &mut self.entries[index];
        let fresh = matches!(slot, Some(done) if done.width == width && done.len == text.len());
        if !fresh {
            *slot = Some(Markdown {
                width,
                len: text.len(),
                parts: markdown(text, width),
            });
        }
        slot.as_ref().map_or(&[], |done| &done.parts)
    }
}

/// `text` laid out one block at a time. Prose goes at `width`. A code block
/// or table keeps its natural width, so one wider than `width` is a
/// `Part::Wide` for the screen to window.
fn markdown(text: &str, width: usize) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    for (block, item) in cmr::parse(text).iter().enumerate() {
        if block > 0 {
            flow(&mut parts).push(Line::default());
        }
        match wide(item, width) {
            Some((rows, natural)) => parts.push(Part::Wide {
                block,
                rows,
                natural,
            }),
            None => flow(&mut parts).extend(block_rows(item, width)),
        }
    }
    parts
}

fn flow(parts: &mut Vec<Part>) -> &mut Vec<Line<'static>> {
    if !matches!(parts.last(), Some(Part::Flow(_))) {
        parts.push(Part::Flow(Vec::new()));
    }
    match parts.last_mut() {
        Some(Part::Flow(rows)) => rows,
        _ => unreachable!(),
    }
}

fn block_rows(block: &cmr::Block, width: usize) -> Vec<Line<'static>> {
    cmr::layout(std::slice::from_ref(block), width)
        .into_iter()
        .map(|row| {
            trim_end(
                row.into_iter()
                    .map(|(text, style)| Span::styled(text, screen_style(style)))
                    .collect(),
            )
        })
        .collect()
}

/// The rows and natural width of a code block or table wider than `width`.
fn wide(block: &cmr::Block, width: usize) -> Option<(Vec<Line<'static>>, usize)> {
    let (rows, natural) = match block {
        cmr::Block::Code { lang, lines } => {
            let longest = lines.iter().map(|line| line.chars().count());
            let header = lang.iter().map(|lang| lang.chars().count());

            // the gutter is two columns: `│ ` on a line, `┌ ` before the language
            let natural = longest.chain(header).max().unwrap_or(0) + 2;
            if natural <= width {
                return None;
            }
            (block_rows(block, natural), natural)
        }
        cmr::Block::Table { .. } => {
            let rows = block_rows(block, TABLE_WIDTH);
            let natural = rows.iter().map(Line::width).max().unwrap_or(0);
            if natural <= width {
                return None;
            }
            (rows, natural)
        }
        _ => return None,
    };
    Some((rows, natural))
}

/// `row` cut to the `visible` columns from `offset`, with a dim `◂` in the
/// first column when there is more to the left and a dim `▸` in the last when
/// there is more to the right of a row `natural` columns wide.
fn window(row: &Line<'static>, offset: usize, visible: usize, natural: usize) -> Line<'static> {
    let mut cells: Vec<(char, Style)> = row
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(|ch| (ch, span.style)))
        .skip(offset)
        .take(visible)
        .collect();
    cells.resize(visible, (' ', Style::default()));
    if offset > 0 {
        cells[0] = ('◂', dim());
    }
    if offset + visible < natural {
        cells[visible - 1] = ('▸', dim());
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (ch, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), style)),
        }
    }
    Line::from(spans)
}

fn screen_style(style: cmr::Style) -> Style {
    let mut out = Style::new();
    if let Some(color) = style.fg {
        let palette = palette::get();
        out = out.fg(match color {
            cmr::Color::Black => Color::Black,
            cmr::Color::Red => palette.red,
            cmr::Color::Green => palette.green,
            cmr::Color::Yellow => palette.yellow,
            cmr::Color::Blue => palette.blue,
            cmr::Color::Magenta => palette.magenta,
            cmr::Color::Cyan => palette.cyan,
            cmr::Color::Gray => Color::Gray,
            cmr::Color::DarkGray => palette.muted.unwrap_or(Color::DarkGray),
            cmr::Color::LightRed => palette.orange,
            cmr::Color::LightGreen => palette.green,
            cmr::Color::LightYellow => palette.yellow,
            cmr::Color::LightBlue => palette.blue,
            cmr::Color::LightMagenta => palette.magenta,
            cmr::Color::LightCyan => palette.cyan,
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
    let usable = width - width / 10;
    let mut lines: Vec<Line> = Vec::new();
    let mut wides: Vec<Wide> = Vec::new();
    let mut folds: Vec<Fold> = Vec::new();
    for (index, entry) in app.transcript.entries.iter().enumerate() {
        match entry {
            Entry::User(text) => lines.extend(bubble(text, width)),
            Entry::Agent(text) => {
                let widest = (width * 9 / 10).max(5);
                let visible = widest - 4;
                let mut rows: Vec<Line<'static>> = Vec::new();
                for part in app.rendered.agent(index, text, visible) {
                    match part {
                        Part::Flow(flow) => rows.extend(flow.iter().cloned()),
                        Part::Wide {
                            block,
                            rows: wide,
                            natural,
                        } => {
                            let max = natural - visible;
                            let key = (index, *block);
                            let offset = app.pans.get(&key).copied().unwrap_or(0).min(max);
                            wides.push(Wide {
                                key,
                                // one row for the border above the box's rows
                                first: lines.len() + 1 + rows.len(),
                                height: wide.len(),
                                width: visible,
                                max,
                            });
                            rows.extend(
                                wide.iter()
                                    .map(|row| window(row, offset, visible, *natural)),
                            );
                        }
                    }
                }
                lines.extend(framed(rows, palette::get().blue, None));
            }
            Entry::Thought(text) => wrapped("✻ ", "  ", dim(), text, usable, dim(), &mut lines),
            Entry::Tool(tool) if !tool.children.is_empty() => {
                folds.push(Fold {
                    id: tool.id.clone(),
                    first: lines.len(),
                });
                let expanded = app.expanded.contains(&tool.id);
                lines.extend(subagent_block(tool, &app.cwd, usable, expanded));
            }
            Entry::Tool(tool) => lines.extend(tool_block(tool, &app.cwd, usable)),
            Entry::Permission { choice, .. } => {
                let pad = width.saturating_sub(choice.chars().count());
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(choice.clone(), Style::new().fg(palette::get().accent)),
                ]));
            }
            Entry::Notice(text) => {
                let text = cut(text, width);
                let pad = (width - text.chars().count()) / 2;
                lines.push(Line::from(format!("{}{text}", " ".repeat(pad))).style(dim()));
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
    let start = end.saturating_sub(rows);
    app.folds = folds
        .iter()
        .filter_map(|fold| {
            let top = fold.first.max(start);
            let bottom = (fold.first + 2).min(end);
            (top < bottom).then(|| FoldArea {
                area: Rect {
                    x: area.x,
                    y: area.y + (top - start) as u16,
                    width: area.width,
                    height: (bottom - top) as u16,
                },
                id: fold.id.clone(),
            })
        })
        .collect();
    app.wide = wides
        .iter()
        .filter_map(|wide| {
            let top = wide.first.max(start);
            let bottom = (wide.first + wide.height).min(end);
            (top < bottom).then(|| WideBlock {
                area: Rect {
                    x: area.x + 2,
                    y: area.y + (top - start) as u16,
                    width: wide.width as u16,
                    height: (bottom - top) as u16,
                },
                key: wide.key,
                max: wide.max,
            })
        })
        .collect();
    let mut visible: Vec<Line> = lines.into_iter().take(end).skip(start).collect();
    if app.scroll > 0 {
        visible.push(Line::styled(format!("⋯ {} more below", app.scroll), dim()));
    }
    frame.render_widget(Paragraph::new(visible), area);
}

/// A subagent block drawn this frame, by the line it starts on.
struct Fold {
    id: ToolCallId,
    first: usize,
}

/// A subagent block's two header rows on screen, which a click folds or
/// unfolds.
pub struct FoldArea {
    pub area: Rect,
    pub id: ToolCallId,
}

/// A wide block drawn this frame, by its place among the transcript lines.
struct Wide {
    key: (usize, usize),
    first: usize,
    height: usize,
    width: usize,
    max: usize,
}

/// A wide block on screen, found by the mouse to pan it.
pub struct WideBlock {
    pub area: Rect,

    /// The entry and the block within it.
    pub key: (usize, usize),

    /// The furthest the block pans, in columns.
    pub max: usize,
}

/// A message of yours in an accent bubble against the right edge, at most
/// 70% of `width` wide.
fn bubble(text: &str, width: usize) -> Vec<Line<'static>> {
    let widest = (width * 7 / 10).max(5);
    let mut rows: Vec<Line> = Vec::new();
    for paragraph in text.split('\n') {
        let pieces = textwrap::wrap(paragraph, widest - 4);
        if pieces.is_empty() {
            rows.push(Line::default());
        }
        rows.extend(
            pieces
                .into_iter()
                .map(|piece| Line::from(piece.into_owned())),
        );
    }
    framed(rows, palette::get().accent, Some(width))
}

/// `rows` inside a rounded border of colour `border` with a space of padding
/// each side. With `Some(width)` the box sits against the right edge of that
/// width, and otherwise against the left.
fn framed(
    rows: Vec<Line<'static>>,
    border: Color,
    right_edge: Option<usize>,
) -> Vec<Line<'static>> {
    let inner = rows.iter().map(Line::width).max().unwrap_or(0);
    let pad = right_edge.map_or(0, |width| width.saturating_sub(inner + 4));
    let edge = Style::new().fg(border);
    let side = |left: &str, middle: &str, right: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(pad)),
            Span::styled(format!("{left}{}{right}", middle.repeat(inner + 2)), edge),
        ])
    };
    let mut lines = vec![side("╭", "─", "╮")];
    for row in rows {
        let gap = inner - row.width();
        let mut spans = vec![Span::raw(" ".repeat(pad)), Span::styled("│ ", edge)];
        spans.extend(row.spans);
        spans.push(Span::styled(format!("{} │", " ".repeat(gap)), edge));
        lines.push(Line::from(spans));
    }
    lines.push(side("╰", "─", "╯"));
    lines
}

/// `line` without the spaces that end it.
fn trim_end(mut line: Line<'static>) -> Line<'static> {
    while let Some(last) = line.spans.last_mut() {
        let kept = last.content.trim_end().len();
        if kept > 0 {
            last.content = last.content[..kept].to_string().into();
            break;
        }
        line.spans.pop();
    }
    line
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
    tool_rows(tool, cwd, width, kind_word(tool.kind), "")
}

/// A tool call's two rows, with `word` in the kind column and `suffix` dim
/// after the title.
fn tool_rows(
    tool: &Tool,
    cwd: &Path,
    width: usize,
    word: &str,
    suffix: &str,
) -> [Line<'static>; 2] {
    let (dot, status, status_style) = match tool.status {
        ToolCallStatus::InProgress => (
            Style::new().fg(palette::get().yellow),
            "running…",
            Style::new().fg(palette::get().yellow),
        ),
        ToolCallStatus::Completed => (Style::new().fg(palette::get().green), "done", dim()),
        ToolCallStatus::Failed => (
            Style::new().fg(palette::get().red),
            "failed",
            Style::new().fg(palette::get().red),
        ),
        _ => (dim(), "pending", dim()),
    };
    let head = [
        Span::styled("● ", dot),
        Span::styled(format!("{word:<6}"), bold()),
        Span::raw(" "),
    ];
    let room =
        width.saturating_sub(head.iter().map(Span::width).sum::<usize>() + suffix.chars().count());
    let mut first = head.to_vec();
    first.push(Span::raw(cut(&tool.title, room)));
    first.push(Span::styled(suffix.to_string(), dim()));

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
        detail.push(Span::styled(plus, Style::new().fg(palette::get().green)));
        detail.push(Span::styled(minus, Style::new().fg(palette::get().red)));
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

/// A subagent's call: folded to its two rows, or expanded with what the
/// subagent did indented under it.
fn subagent_block(tool: &Tool, cwd: &Path, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let count = tool.tool_count();
    let noun = if count == 1 { "tool" } else { "tools" };
    let suffix = format!("  {count} {noun}");
    let mut lines = tool_rows(tool, cwd, width, "agent", &suffix).to_vec();
    if !expanded {
        return lines;
    }
    let inner = width.saturating_sub(SUBAGENT_INDENT.len());
    for child in &tool.children {
        let mut rows: Vec<Line<'static>> = match child {
            Child::Tool(call) => tool_block(call, cwd, inner).to_vec(),
            Child::Text(text) => {
                let mut rows = Vec::new();
                wrapped(
                    "",
                    "",
                    Style::default(),
                    text,
                    inner,
                    Style::default(),
                    &mut rows,
                );
                rows
            }
            Child::Thought(text) => {
                let mut rows = Vec::new();
                wrapped("✻ ", "  ", dim(), text, inner, dim(), &mut rows);
                rows
            }
        };
        for row in &mut rows {
            row.spans.insert(0, Span::raw(SUBAGENT_INDENT));
        }
        lines.extend(rows);
    }
    lines
}

const SUBAGENT_INDENT: &str = "    ";

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
            Span::styled(format!("+{added}"), Style::new().fg(palette::get().green)),
            Span::styled(format!(" −{removed}"), Style::new().fg(palette::get().red)),
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
    frame.render_widget(
        Paragraph::new(lines).block(
            pane(&title, true).title_bottom(Line::styled("esc cancel", dim()).right_aligned()),
        ),
        area,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, input: &InputView, colour: Option<Color>) {
    let text = colour.map_or(Style::default(), |colour| Style::new().fg(colour));
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
            Line::from(vec![lead, Span::styled(row.clone(), text)])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    let (column, row) = input.cursor;
    frame.set_cursor_position((area.x + column, area.y + row));
}

/// The colour of the prompt's text: cyan for a coax command, green for an
/// agent command.
fn prompt_colour(input: &str) -> Option<Color> {
    match command::parse(input) {
        command::Typed::Plain => None,
        command::Typed::Coax { .. } => Some(palette::get().cyan),
        command::Typed::Agent { .. } => Some(palette::get().green),
    }
}

/// The commands matching the prompt's text, in a box above the prompt.
fn draw_commands(frame: &mut Frame, app: &App, above: Rect) {
    let list = app.popup();
    if list.is_empty() {
        return;
    }
    let colour = prompt_colour(&app.input).unwrap_or_default();
    let screen = frame.area();
    let shown = list.len().min(6).min((above.y as usize).saturating_sub(2));
    if shown == 0 {
        return;
    }
    let cursor = app.command_cursor.min(list.len() - 1);
    let first = (cursor + 1).saturating_sub(shown);
    let width = screen.width.min(70);
    let area = Rect {
        x: 0,
        y: above.y - shown as u16 - 2,
        width,
        height: shown as u16 + 2,
    };
    let inner = (width as usize).saturating_sub(4);
    let name_width = list
        .iter()
        .map(|entry| entry.fill.trim_end().chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(shown)
        .map(|(index, entry)| {
            let name = format!("{:<name_width$}", entry.fill.trim_end());
            let room = inner.saturating_sub(name_width + 2);
            let line = Line::from(vec![
                Span::styled(name, Style::new().fg(colour)),
                Span::styled(format!("  {}", cut(&entry.description, room)), dim()),
            ]);
            if index == cursor {
                line.style(Style::new().bg(palette::get().selection))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(pane("commands", false)), area);
}

/// The box that asks before an agent command is sent.
fn draw_confirm(frame: &mut Frame, over: Rect, app: &App) {
    let Some(confirm) = &app.confirm else {
        return;
    };
    let text = confirm.text();
    let mut lines = vec![Line::styled(
        text.clone(),
        Style::new().fg(palette::get().green),
    )];
    lines.push(Line::styled(confirm.description.clone(), dim()));
    let wanted = lines.iter().map(Line::width).max().unwrap_or(0).max(24) + 4;
    let width = (wanted as u16).min(over.width);
    let height = (lines.len() as u16 + 2).min(over.height);
    let area = Rect {
        x: over.x + (over.width - width) / 2,
        y: over.y + (over.height - height) / 2,
        width,
        height,
    };
    let block = pane("send to the agent", true)
        .title_bottom(Line::styled("enter send   esc cancel", dim()).right_aligned());
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
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
const PROMPT_ROWS: usize = 1;

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

/// The picker box hanging under a header chip.
fn draw_menu(frame: &mut Frame, app: &mut App) {
    app.menu_area = None;
    let Some(menu) = &app.menu else {
        return;
    };
    let settings = app.settings.settings();
    let (title, rows): (String, Vec<(String, Option<String>, bool)>) = match &menu.target {
        MenuTarget::Setting(id) => {
            let Some(setting) = settings.iter().find(|setting| setting.id == *id) else {
                return;
            };
            let Control::Choose(choices) = &setting.control else {
                return;
            };
            let rows = choices
                .iter()
                .map(|choice| {
                    (
                        choice.name.clone(),
                        choice.description.clone(),
                        choice.current,
                    )
                })
                .collect();
            (setting.name.clone(), rows)
        }
        MenuTarget::Overflow(first) => {
            let rows = settings
                .iter()
                .skip(*first)
                .map(|setting| (setting.label.clone(), None, false))
                .collect();
            ("settings".to_string(), rows)
        }
    };
    let screen = frame.area();
    let row_width = |(name, note, _): &(String, Option<String>, bool)| {
        2 + name.chars().count() + note.as_ref().map_or(0, |note| 2 + note.chars().count())
    };
    let wanted = rows
        .iter()
        .map(row_width)
        .max()
        .unwrap_or(0)
        .max(title.chars().count() + 2)
        + 4;
    let width = (wanted as u16).min(60).min(screen.width);
    let warning = menu.confirm.as_ref().and_then(|value| match &menu.target {
        MenuTarget::Setting(id) => {
            let setting = settings.iter().find(|setting| setting.id == *id)?;
            let Control::Choose(choices) = &setting.control else {
                return None;
            };
            let choice = choices.iter().find(|choice| choice.value == *value)?;
            Some(settings::rereads(
                &choice.name,
                app.settings.context.as_ref().map(|c| c.used),
            ))
        }
        MenuTarget::Overflow(_) => None,
    });
    let width = if warning.is_some() {
        width.max(60.min(screen.width))
    } else {
        width
    };
    let inner = (width as usize).saturating_sub(4);
    let warning_rows: Vec<String> = warning
        .iter()
        .flat_map(|text| textwrap::wrap(text, inner.max(1)))
        .map(|row| row.into_owned())
        .collect();
    let error = usize::from(menu.error.is_some()) + warning_rows.len();
    let visible = rows
        .len()
        .min((screen.height as usize).saturating_sub(4 + error))
        .max(1);
    let first = (menu.cursor + 1).saturating_sub(visible);
    let area = Rect {
        x: menu.anchor.min(screen.width - width),
        y: 1,
        width,
        height: (visible + error) as u16 + 2,
    };
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(index, (name, note, current))| {
            let mark = if *current { "● " } else { "  " };
            let mut spans = vec![Span::raw(mark), Span::raw(name.clone())];
            if let Some(note) = note {
                let room = inner.saturating_sub(2 + name.chars().count() + 2);
                spans.push(Span::styled(format!("  {}", cut(note, room)), dim()));
            }
            let line = Line::from(spans);
            if index == menu.cursor {
                line.style(Style::new().bg(palette::get().selection))
            } else {
                line
            }
        })
        .collect();
    for row in warning_rows {
        lines.push(Line::styled(row, Style::new().fg(palette::get().yellow)));
    }
    if let Some(message) = &menu.error {
        lines.push(Line::styled(cut(message, inner), dim()));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(pane(&title, true)), area);
    app.menu_area = Some(area);
    app.menu_first = first;
}

pub fn dim() -> Style {
    match palette::get().muted {
        Some(color) => Style::new().fg(color),
        None => Style::default().add_modifier(Modifier::DIM),
    }
}

fn focus() -> Style {
    Style::new().fg(palette::get().blue)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{Diff, ToolCallLocation};

    use super::*;

    fn flat(parts: &[Part]) -> Vec<Line<'static>> {
        parts
            .iter()
            .flat_map(|part| match part {
                Part::Flow(rows) | Part::Wide { rows, .. } => rows.clone(),
            })
            .collect()
    }

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect()
    }

    #[test]
    fn agent_text_is_laid_out_again_when_it_grows_or_the_width_changes() {
        let mut rendered = Rendered::default();
        assert_eq!(plain(&flat(rendered.agent(0, "one two", 20))), ["one two"]);
        assert_eq!(
            plain(&flat(rendered.agent(0, "one two three", 20))),
            ["one two three"]
        );
        assert_eq!(
            plain(&flat(rendered.agent(0, "one two three", 8))),
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
            children: Vec::new(),
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
        let lines = flat(&markdown("# Title\n\nsome `code`", 40));
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
    fn the_prompt_shows_one_row_for_short_text_and_grows_with_long_text() {
        assert_eq!(input_view("", 20, 10).height(), 1);
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

    #[test]
    fn a_short_message_gets_a_bubble_as_wide_as_its_text_plus_four() {
        let lines = plain(&bubble("hi", 40));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], format!("{}╭────╮", " ".repeat(34)));
        assert_eq!(lines[1], format!("{}│ hi │", " ".repeat(34)));
        assert_eq!(lines[2], format!("{}╰────╯", " ".repeat(34)));
    }

    #[test]
    fn a_bubble_sits_flush_against_the_right_edge_at_any_width() {
        for width in [20, 60, 100] {
            for line in bubble("hello there", width) {
                assert_eq!(line.width(), width);
            }
        }
    }

    #[test]
    fn a_long_message_wraps_inside_a_bubble_of_seventy_percent_of_the_width() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let lines = bubble(text, 40);
        assert!(lines.len() > 3);
        for line in &lines {
            assert_eq!(line.width(), 40);
            assert!(line.to_string().trim_start().chars().count() <= 28);
        }
        let first = lines[0].to_string();
        assert_eq!(first.trim_start().chars().count(), 28);
        assert!(
            plain(&lines[1..lines.len() - 1])
                .iter()
                .all(|row| row.trim_start().starts_with('│') && row.ends_with('│'))
        );
    }

    #[test]
    fn an_agent_box_is_sized_to_its_longest_line_without_trailing_padding() {
        let rows = vec![
            trim_end(Line::from("short   ")),
            trim_end(Line::from("a longer line   ")),
        ];
        let lines = plain(&framed(rows, Color::Blue, None));
        assert_eq!(lines[0], "╭───────────────╮");
        assert_eq!(lines[1], "│ short         │");
        assert_eq!(lines[2], "│ a longer line │");
        assert_eq!(lines[3], "╰───────────────╯");
    }

    #[test]
    fn a_bubble_border_takes_the_given_colour_and_has_no_background() {
        let lines = framed(vec![Line::from("hi")], Color::Red, Some(20));
        for span in lines.iter().flat_map(|line| &line.spans) {
            assert_eq!(span.style.bg, None);
        }
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_code_block_is_as_wide_as_its_longest_line_plus_the_gutter() {
        let parts = markdown("```rs\nabcdefghij\nabc\n```", 8);
        let [Part::Wide { natural, rows, .. }] = &parts[..] else {
            panic!("expected one wide block");
        };
        assert_eq!(*natural, 12);
        assert_eq!(plain(rows), ["┌ rs", "│ abcdefghij", "│ abc"]);
    }

    #[test]
    fn a_table_is_as_wide_as_its_longest_row_with_the_padding_trimmed() {
        let text = "| name | note |\n|---|---|\n| a | a rather long note indeed |";
        let parts = markdown(text, 12);
        let [Part::Wide { natural, rows, .. }] = &parts[..] else {
            panic!("expected one wide block");
        };
        let widest = rows.iter().map(Line::width).max().unwrap();
        assert_eq!(*natural, widest);
        assert!(widest > 12 && widest < 40, "{widest}");
    }

    #[test]
    fn a_block_that_fits_the_width_is_laid_out_with_the_prose() {
        let parts = markdown("```\nshort\n```\n\nafter", 20);
        assert!(matches!(&parts[..], [Part::Flow(_)]));
    }

    #[test]
    fn a_window_marks_the_edges_that_have_more_beyond_them() {
        let row = Line::from("0123456789");
        let at = |offset| window(&row, offset, 5, 10).to_string();
        assert_eq!(at(0), "0123▸");
        assert_eq!(at(2), "◂345▸");
        assert_eq!(at(5), "◂6789");
    }

    #[test]
    fn a_window_pads_a_row_shorter_than_the_block() {
        let row = Line::from("01");
        assert_eq!(window(&row, 0, 5, 10).to_string(), "01  ▸");
        assert_eq!(window(&row, 3, 5, 10).to_string(), "◂   ▸");
    }

    #[test]
    fn laying_out_a_block_at_a_time_matches_laying_out_the_whole_text() {
        let text = "# Title\n\nSome *prose* with `code` that wraps around a little.\n\n\
                    - one\n- two\n\n```rs\nfn main() {}\n```\n\n\
                    | a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n> quoted text";
        for width in [30, 60] {
            let whole: Vec<Line> = cmr::render(text, width)
                .into_iter()
                .map(|row| {
                    trim_end(
                        row.into_iter()
                            .map(|(text, style)| Span::styled(text, screen_style(style)))
                            .collect(),
                    )
                })
                .collect();
            let parts = markdown(text, width);
            assert!(matches!(&parts[..], [Part::Flow(_)]));
            assert_eq!(flat(&parts), whole);
        }
    }

    fn task() -> Tool {
        let mut task = tool("Survey the entry point", "/work/x.rs", None);
        task.kind = ToolKind::Think;
        task.status = ToolCallStatus::InProgress;
        task.locations.clear();
        task.children = vec![
            Child::Tool(tool("Read src/main.rs", "/work/src/main.rs", None)),
            Child::Text("main.rs is a stub.".to_string()),
        ];
        task
    }

    #[test]
    fn a_folded_subagent_block_shows_its_title_its_tool_count_and_its_status() {
        let lines = plain(&subagent_block(&task(), Path::new("/work"), 60, false));
        assert_eq!(
            lines,
            ["● agent  Survey the entry point  1 tool", "  ⎿  running…"]
        );
    }

    #[test]
    fn an_expanded_subagent_block_draws_its_children_indented_four_columns() {
        let lines = plain(&subagent_block(&task(), Path::new("/work"), 60, true));
        assert_eq!(
            lines,
            [
                "● agent  Survey the entry point  1 tool",
                "  ⎿  running…",
                "    ● read   Read src/main.rs",
                "      ⎿  done",
                "    main.rs is a stub.",
            ]
        );
    }

    #[test]
    fn the_prompt_is_cyan_for_a_coax_command_and_green_for_an_agent_command() {
        let palette = palette::get();
        assert_eq!(prompt_colour("/mode"), Some(palette.cyan));
        assert_eq!(prompt_colour("/"), Some(palette.cyan));
        assert_eq!(prompt_colour("//compact"), Some(palette.green));
        assert_eq!(prompt_colour("//"), Some(palette.green));
        assert_eq!(prompt_colour("hello /mode"), None);
        assert_eq!(prompt_colour(""), None);
    }
}
