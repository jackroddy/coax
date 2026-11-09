use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use agent_client_protocol::v1::schema::{
    ContentBlock, PlanEntryStatus, ToolCallContent, ToolCallId, ToolCallStatus, ToolKind,
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
        app.cursor,
        (area.width as usize).saturating_sub(PROMPT_LEAD.len()),
        (area.height / 2) as usize,
    );
    let plan_rows = if app.show_plan {
        app.transcript.plan.len().min(MAX_PLAN_ROWS)
    } else {
        0
    };
    let [
        transcript_area,
        plan_area,
        upper_rule,
        prompt_area,
        lower_rule,
        status_area,
    ] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(plan_rows as u16),
        Constraint::Length(1),
        Constraint::Length(input.height() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, transcript_area, app);
    draw_plan(frame, plan_area, app);
    draw_rule(frame, upper_rule);
    draw_input(frame, prompt_area, &input, prompt_colour(&app.input));
    draw_commands(frame, app, upper_rule);
    draw_confirm(frame, transcript_area, app);
    draw_guard_rule(frame, lower_rule, app);
    header::draw_status(frame, status_area, app);
    draw_menu(frame, app);
}

/// The rule under the prompt, carrying the exit guard's message while armed.
fn draw_guard_rule(frame: &mut Frame, area: Rect, app: &App) {
    let Some(guard) = app.guard else {
        return match &app.unknown {
            Some(message) => draw_notice_rule(frame, area, message),
            None => draw_rule(frame, area),
        };
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

/// The rule under the prompt, carrying a dim `message`.
fn draw_notice_rule(frame: &mut Frame, area: Rect, message: &str) {
    let rest = (area.width as usize).saturating_sub(message.chars().count() + 4);
    frame.render_widget(
        Paragraph::new(format!("── {message} {}", "─".repeat(rest))).style(dim()),
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
/// table kept at its natural width.
enum Part {
    Flow(Vec<Line<'static>>),
    Block {
        /// Which block of the entry this is.
        block: usize,

        /// The rows at the block's natural width, trailing spaces trimmed.
        rows: Vec<Line<'static>>,
        natural: usize,

        /// The text a copy takes, for a literal block.
        copy: Option<String>,
    },
}

/// Columns a code block keeps free after its longest line, where the copy
/// glyph sits on the first row.
const COPY_ROOM: usize = 2;

/// The copy glyph, from the Nerd Font set that the header's separators come
/// from.
const COPY_GLYPH: &str = "\u{f0c5}";

/// The widest a table is laid out when measuring it.
const TABLE_WIDTH: usize = 1000;

impl Rendered {
    /// The text a copy of block `key` (an entry and a block within it) takes.
    pub fn copy_text(&self, key: (usize, usize)) -> Option<&str> {
        let markdown = self.entries.get(key.0)?.as_ref()?;
        markdown.parts.iter().find_map(|part| match part {
            Part::Block {
                block,
                copy: Some(text),
                ..
            } if *block == key.1 => Some(text.as_str()),
            _ => None,
        })
    }

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
/// keeps its natural width, as does a table wider than `width`. Either is a
/// `Part::Block`, and the screen windows one wider than `width`.
fn markdown(text: &str, width: usize) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    for (block, item) in cmr::parse(text).iter().enumerate() {
        if block > 0 {
            flow(&mut parts).push(Line::default());
        }
        match kept_whole(item, width) {
            Some((rows, natural, copy)) => parts.push(Part::Block {
                block,
                rows,
                natural,
                copy,
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

/// The rows, natural width and copy text of a code block, or the rows and
/// width of a table wider than `width`.
fn kept_whole(
    block: &cmr::Block,
    width: usize,
) -> Option<(Vec<Line<'static>>, usize, Option<String>)> {
    match block {
        cmr::Block::Code { lang, lines } => {
            let longest = lines.iter().map(|line| line.chars().count());
            let header = lang.iter().map(|lang| lang.chars().count());

            // the gutter is two columns: `│ ` on a line, `┌ ` before the language
            let natural = longest.chain(header).max().unwrap_or(0) + 2 + COPY_ROOM;
            Some((block_rows(block, natural), natural, Some(lines.join("\n"))))
        }
        cmr::Block::Table { .. } => {
            let rows = block_rows(block, TABLE_WIDTH);
            let natural = rows.iter().map(Line::width).max().unwrap_or(0);
            (natural > width).then_some((rows, natural, None))
        }
        _ => None,
    }
}

/// `row` cut to the `visible` columns from `offset`.
fn window(row: &Line<'static>, offset: usize, visible: usize) -> Line<'static> {
    cells_line(window_cells(row, offset, visible))
}

fn window_cells(row: &Line<'static>, offset: usize, visible: usize) -> Vec<(char, Style)> {
    let mut cells: Vec<(char, Style)> = row
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(|ch| (ch, span.style)))
        .skip(offset)
        .take(visible)
        .collect();
    cells.resize(visible, (' ', Style::default()));
    cells
}

/// `window` with `label` in the last columns.
fn window_with_corner(
    row: &Line<'static>,
    offset: usize,
    visible: usize,
    label: &str,
    style: Style,
) -> Line<'static> {
    let mut cells = window_cells(row, offset, visible);
    let label: Vec<char> = label.chars().take(visible).collect();
    for (n, ch) in label.iter().enumerate() {
        cells[visible - label.len() + n] = (*ch, style);
    }
    cells_line(cells)
}

/// The thumb of a scroll bar `visible` columns wide over a block that pans
/// `max` columns, at pan `offset`: its first column and its length. The
/// length is the visible share of the block, at least `min_len`.
pub fn thumb(visible: usize, max: usize, offset: usize, min_len: usize) -> (usize, usize) {
    let natural = visible + max;
    let len = ((visible * visible + natural / 2) / natural).clamp(min_len.min(visible), visible);
    let free = visible - len;
    let start = (offset.min(max) * free + max / 2)
        .checked_div(max)
        .unwrap_or(0);
    (start, len)
}

/// The pan that puts the thumb's first column at `start`, which may lie
/// outside the bar: the inverse of `thumb`.
pub fn pan_for_thumb(visible: usize, max: usize, start: isize, min_len: usize) -> usize {
    let free = visible - thumb(visible, max, 0, min_len).1;
    if free == 0 {
        return 0;
    }
    let start = start.clamp(0, free as isize) as usize;
    (start * max + free / 2) / free
}

/// The scroll bar row under a wide block: a dim track with a thumb on it,
/// in the accent colour while `hot`.
fn scroll_bar(visible: usize, max: usize, offset: usize, hot: bool) -> Line<'static> {
    let (start, len) = thumb(visible, max, offset, 2);
    let colour = if hot {
        palette::get().accent
    } else {
        palette::get().foreground
    };
    Line::from(vec![
        Span::styled("─".repeat(start), dim()),
        Span::styled("━".repeat(len), Style::new().fg(colour)),
        Span::styled("─".repeat(visible - start - len), dim()),
    ])
}

fn cells_line(cells: Vec<(char, Style)>) -> Line<'static> {
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

/// The transcript laid out at one width.
struct Built {
    lines: Vec<Line<'static>>,
    wides: Vec<Wide>,
    copies: Vec<Copy>,
    folds: Vec<Fold>,
}

fn build_transcript(app: &mut App, width: usize) -> Built {
    let usable = width - width / 10;
    let mut lines: Vec<Line> = Vec::new();
    let mut wides: Vec<Wide> = Vec::new();
    let mut copies: Vec<Copy> = Vec::new();
    let mut folds: Vec<Fold> = Vec::new();
    let copied = app
        .copied
        .as_ref()
        .filter(|(_, until)| *until > Instant::now())
        .map(|(key, _)| *key);
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
                        Part::Block {
                            block,
                            rows: block_rows,
                            natural,
                            copy,
                        } => {
                            let shown = (*natural).min(visible);
                            let max = natural - shown;
                            let key = (index, *block);
                            let offset = app.pans.get(&key).copied().unwrap_or(0).min(max);

                            // one row for the border above the box's rows
                            let first = lines.len() + 1 + rows.len();
                            let hot = app.hover_bar == Some(key)
                                || app.drag.is_some_and(|drag| drag.key == key);
                            if max > 0 {
                                wides.push(Wide {
                                    key,
                                    first,
                                    height: block_rows.len(),
                                    width: shown,
                                    max,
                                });
                            }
                            if copy.is_some() {
                                copies.push(Copy {
                                    key,
                                    first,
                                    width: shown,
                                });
                            }
                            for (n, row) in block_rows.iter().enumerate() {
                                if n > 0 || copy.is_none() {
                                    rows.push(window(row, offset, shown));
                                    continue;
                                }
                                let (label, style) = if copied == Some(key) {
                                    ("copied", Style::new().fg(palette::get().accent))
                                } else if app.hover_copy == Some(key) {
                                    (COPY_GLYPH, Style::new().fg(palette::get().accent))
                                } else {
                                    (COPY_GLYPH, dim())
                                };
                                rows.push(window_with_corner(row, offset, shown, label, style));
                            }
                            if max > 0 {
                                rows.push(scroll_bar(shown, max, offset, hot));
                            }
                        }
                    }
                }
                lines.extend(framed(rows, palette::get().blue, None));
            }
            Entry::Thought(text) => wrapped("✻ ", "  ", dim(), text, usable, dim(), &mut lines),
            Entry::Tool(tool) => {
                let expanded = app.expanded.contains(&tool.id);
                let hovered = app.hover_fold.as_ref() == Some(&tool.id);
                let (rows, head) = tool_view(tool, &app.cwd, usable, expanded, hovered);
                if expandable(tool) {
                    folds.push(Fold {
                        id: tool.id.clone(),
                        first: lines.len(),
                        height: head,
                    });
                }
                lines.extend(rows);
            }
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
    Built {
        lines,
        wides,
        copies,
        folds,
    }
}

/// The conversation, one column narrower with a scroll bar in the last
/// column when it is taller than `outer`.
fn draw_transcript(frame: &mut Frame, outer: Rect, app: &mut App) {
    let height = outer.height as usize;
    let mut area = outer;
    let mut built = build_transcript(app, (area.width as usize).max(4));
    if built.lines.len() > height && area.width > 1 {
        area.width -= 1;
        built = build_transcript(app, (area.width as usize).max(4));
    }
    let Built {
        lines,
        wides,
        copies,
        folds,
    } = built;
    app.page = height.saturating_sub(1).max(1);
    if app.scroll > 0 {
        app.scroll += lines.len().saturating_sub(app.lines_seen);
    }
    app.lines_seen = lines.len();
    app.scroll = app.scroll.min(lines.len().saturating_sub(height));
    let end = lines.len() - app.scroll;
    let start = end.saturating_sub(height);
    app.folds = folds
        .iter()
        .filter_map(|fold| {
            let top = fold.first.max(start);
            let bottom = (fold.first + fold.height).min(end);
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
    app.copies = copies
        .iter()
        .filter(|copy| (start..end).contains(&copy.first))
        .map(|copy| CopyArea {
            // the glyph and the column before it
            area: Rect::new(
                area.x + 2 + copy.width.saturating_sub(COPY_ROOM) as u16,
                area.y + (copy.first - start) as u16,
                COPY_ROOM.min(copy.width) as u16,
                1,
            ),
            key: copy.key,
        })
        .collect();
    app.bars = wides
        .iter()
        .filter(|wide| (start..end).contains(&(wide.first + wide.height)))
        .map(|wide| ScrollBar {
            area: Rect::new(
                area.x + 2,
                area.y + (wide.first + wide.height - start) as u16,
                wide.width as u16,
                1,
            ),
            key: wide.key,
            max: wide.max,
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
    let visible: Vec<Line> = lines.into_iter().take(end).skip(start).collect();
    frame.render_widget(Paragraph::new(visible), area);
    app.v_bar = None;
    if area.width < outer.width {
        let bar = VBar {
            area: Rect::new(area.right(), area.y, 1, outer.height),
            max: app.lines_seen - height,
        };
        draw_v_bar(frame, &bar, app);
        app.v_bar = Some(bar);
    }
}

/// The conversation's scroll bar: its column on screen, and how far the
/// conversation scrolls, in lines.
pub struct VBar {
    pub area: Rect,
    pub max: usize,
}

impl VBar {
    /// The thumb's first row and its length for a conversation scrolled up by
    /// `scroll` lines. The thumb sits at the bottom when `scroll` is 0.
    pub fn thumb(&self, scroll: usize) -> (usize, usize) {
        let height = self.area.height as usize;
        thumb(height, self.max, self.max - scroll.min(self.max), 1)
    }

    /// How far up the conversation is scrolled when the thumb's first row is
    /// at `start`, which may lie outside the bar.
    pub fn scroll_for_thumb(&self, start: isize) -> usize {
        let height = self.area.height as usize;
        self.max - pan_for_thumb(height, self.max, start, 1)
    }
}

fn draw_v_bar(frame: &mut Frame, bar: &VBar, app: &App) {
    let (start, len) = bar.thumb(app.scroll);
    let colour = if app.hover_v_bar || app.v_drag.is_some() {
        palette::get().accent
    } else {
        palette::get().foreground
    };
    let rows: Vec<Line> = (0..bar.area.height as usize)
        .map(|row| {
            if (start..start + len).contains(&row) {
                Line::styled("┃", Style::new().fg(colour))
            } else {
                Line::styled("│", dim())
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), bar.area);
}

/// An expandable tool call drawn this frame, by the line it starts on.
struct Fold {
    id: ToolCallId,
    first: usize,

    /// The rows of the call's head, which a click folds or unfolds.
    height: usize,
}

/// A code block drawn this frame, by its first row among the transcript lines.
struct Copy {
    key: (usize, usize),
    first: usize,
    width: usize,
}

/// A copy glyph on screen, found by the mouse.
pub struct CopyArea {
    pub area: Rect,

    /// The entry and the block within it.
    pub key: (usize, usize),
}

/// An expandable tool call's head rows on screen, which a click folds or
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

/// A scroll bar row on screen, found by the mouse to drag its thumb.
pub struct ScrollBar {
    pub area: Rect,

    /// The entry and the block within it.
    pub key: (usize, usize),

    /// The furthest the block pans, in columns.
    pub max: usize,
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

fn tool_block(tool: &Tool, cwd: &Path, width: usize) -> Vec<Line<'static>> {
    tool_rows(tool, cwd, width, kind_word(tool.kind), "")
}

/// A tool call's head: the dot row, and a row under it unless the call is
/// done with nothing to add. `word` goes in the kind column and `suffix` dim
/// after the title.
fn tool_rows(
    tool: &Tool,
    cwd: &Path,
    width: usize,
    word: &str,
    suffix: &str,
) -> Vec<Line<'static>> {
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
        if tool.status == ToolCallStatus::Completed {
            return vec![Line::from(first)];
        }
        second.push(Span::styled(status, status_style));
    }
    vec![Line::from(first), Line::from(second)]
}

/// Whether a click on the call shows more of it: a subagent's work, an edit's
/// diff or a command's output.
fn expandable(tool: &Tool) -> bool {
    !tool.children.is_empty()
        || match tool.kind {
            ToolKind::Edit => tool
                .content
                .iter()
                .any(|content| matches!(content, ToolCallContent::Diff(_))),
            ToolKind::Execute => !output(&tool.content).is_empty(),
            _ => false,
        }
}

/// The text a call's content carries, one string per piece.
fn output(content: &[ToolCallContent]) -> Vec<&str> {
    content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                    Some(text.text.as_str())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A top-level call's rows, and how many of them are its head. Expanded, it
/// has its subagent's work, its diff or its output under the head.
fn tool_view(
    tool: &Tool,
    cwd: &Path,
    width: usize,
    expanded: bool,
    hovered: bool,
) -> (Vec<Line<'static>>, usize) {
    if !expandable(tool) {
        let lines = tool_block(tool, cwd, width);
        let head = lines.len();
        return (lines, head);
    }

    // two columns go to the marker at the end of the title row
    let room = width.saturating_sub(2);
    let (mut lines, head) = if tool.children.is_empty() {
        let lines = tool_block(tool, cwd, room);
        let head = lines.len();
        (lines, head)
    } else {
        let head = subagent_head(tool, cwd, room).len();
        (subagent_block(tool, cwd, room, expanded), head)
    };
    lines[0] = with_marker(lines[0].clone(), width, expanded, hovered);
    if expanded && tool.children.is_empty() {
        let inner = width.saturating_sub(SUBAGENT_INDENT.len());
        let mut rows = match tool.kind {
            ToolKind::Edit => diff_rows(&tool.content, inner),
            _ => output_rows(&tool.content, inner),
        };
        for row in &mut rows {
            row.spans.insert(0, Span::raw(SUBAGENT_INDENT));
        }
        lines.extend(rows);
    }
    (lines, head)
}

/// `title` with the fold marker in column `width`: `▸` folded, `▾` expanded,
/// dim until the block is hovered.
fn with_marker(
    mut title: Line<'static>,
    width: usize,
    expanded: bool,
    hovered: bool,
) -> Line<'static> {
    let style = if hovered {
        Style::new().fg(palette::get().accent)
    } else {
        dim()
    };
    let gap = width.saturating_sub(1 + title.width());
    title.spans.push(Span::raw(" ".repeat(gap)));
    title
        .spans
        .push(Span::styled(if expanded { "▾" } else { "▸" }, style));
    title
}

/// The unchanged lines kept either side of a change in a diff.
const DIFF_CONTEXT: usize = 2;

/// The most cells of the table that finds a diff's common lines. Past it the
/// old lines show as removed and the new as added.
const DIFF_CELLS: usize = 4_000_000;

/// How a line of a diff stands: `' '` unchanged, `'-'` removed, `'+'` added.
fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_mid, new_mid) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );

    let mut out: Vec<(char, &str)> = old[..prefix].iter().map(|line| (' ', *line)).collect();
    if old_mid.len() * new_mid.len() > DIFF_CELLS {
        out.extend(old_mid.iter().map(|line| ('-', *line)));
        out.extend(new_mid.iter().map(|line| ('+', *line)));
    } else {
        // common[i][j] is the length of the longest run of shared lines in
        // old_mid[i..] and new_mid[j..], stored row by row
        let columns = new_mid.len() + 1;
        let mut common = vec![0u32; (old_mid.len() + 1) * columns];
        for i in (0..old_mid.len()).rev() {
            for j in (0..new_mid.len()).rev() {
                common[i * columns + j] = if old_mid[i] == new_mid[j] {
                    common[(i + 1) * columns + j + 1] + 1
                } else {
                    common[(i + 1) * columns + j].max(common[i * columns + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < old_mid.len() && j < new_mid.len() {
            if old_mid[i] == new_mid[j] {
                out.push((' ', old_mid[i]));
                i += 1;
                j += 1;
            } else if common[(i + 1) * columns + j] >= common[i * columns + j + 1] {
                out.push(('-', old_mid[i]));
                i += 1;
            } else {
                out.push(('+', new_mid[j]));
                j += 1;
            }
        }
        out.extend(old_mid[i..].iter().map(|line| ('-', *line)));
        out.extend(new_mid[j..].iter().map(|line| ('+', *line)));
    }
    out.extend(old[old.len() - suffix..].iter().map(|line| (' ', *line)));
    out
}

/// The rows of an edit's diffs: removed lines red after a `-`, added lines
/// green after a `+`, and the unchanged lines around them dim. A run of
/// unchanged lines past the context shows as a count.
fn diff_rows(content: &[ToolCallContent], width: usize) -> Vec<Line<'static>> {
    let diffs: Vec<_> = content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Diff(diff) => Some(diff),
            _ => None,
        })
        .collect();
    let palette = palette::get();
    let mut rows = Vec::new();
    for diff in &diffs {
        if diffs.len() > 1 {
            let path = diff.path.display().to_string();
            rows.push(Line::styled(cut(&path, width), dim()));
        }
        let lines = line_diff(diff.old_text.as_deref().unwrap_or(""), &diff.new_text);
        let near_change = |index: usize| {
            let from = index.saturating_sub(DIFF_CONTEXT);
            let to = (index + DIFF_CONTEXT + 1).min(lines.len());
            lines[from..to].iter().any(|(mark, _)| *mark != ' ')
        };
        let mut shown: Vec<bool> = (0..lines.len()).map(near_change).collect();

        // a count takes a row too, so a lone hidden line is shown instead
        let mut at = 0;
        while at < shown.len() {
            let run = shown[at..].iter().take_while(|s| **s == shown[at]).count();
            if !shown[at] && run == 1 {
                shown[at] = true;
            }
            at += run;
        }
        let mut hidden = 0;
        for (index, (mark, text)) in lines.iter().enumerate() {
            if !shown[index] {
                hidden += 1;
                continue;
            }
            if hidden > 0 {
                rows.push(Line::styled(format!("⋯ {hidden} unchanged"), dim()));
                hidden = 0;
            }
            let text = cut(&format!("{mark} {text}"), width);
            rows.push(match mark {
                '-' => Line::styled(text, Style::new().fg(palette.red)),
                '+' => Line::styled(text, Style::new().fg(palette.green)),
                _ => Line::styled(text, dim()),
            });
        }
        if hidden > 0 {
            rows.push(Line::styled(format!("⋯ {hidden} unchanged"), dim()));
        }
    }
    rows
}

/// The rows of a command's output, laid out as markdown, since the adapter
/// sends it as a fenced block.
fn output_rows(content: &[ToolCallContent], width: usize) -> Vec<Line<'static>> {
    output(content)
        .into_iter()
        .flat_map(|text| cmr::render(text, width))
        .map(|row| {
            trim_end(
                row.into_iter()
                    .map(|(text, style)| Span::styled(text, screen_style(style)))
                    .collect(),
            )
        })
        .collect()
}

/// A subagent's call: folded to its two rows, or expanded with what the
/// subagent did indented under it.
fn subagent_block(tool: &Tool, cwd: &Path, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let mut lines = subagent_head(tool, cwd, width);
    if !expanded {
        return lines;
    }
    let inner = width.saturating_sub(SUBAGENT_INDENT.len());
    for child in &tool.children {
        let mut rows: Vec<Line<'static>> = match child {
            Child::Tool(call) => tool_block(call, cwd, inner),
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

fn subagent_head(tool: &Tool, cwd: &Path, width: usize) -> Vec<Line<'static>> {
    let count = tool.tool_count();
    let noun = if count == 1 { "tool" } else { "tools" };
    tool_rows(tool, cwd, width, "agent", &format!("  {count} {noun}"))
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

fn input_view(text: &str, cursor: usize, width: usize, max_rows: usize) -> InputView {
    let all = wrap_rows(text, width);
    let shown = max_rows.max(PROMPT_ROWS);
    let cursor = cursor.min(text.len());
    let cursor_row = all.iter().rposition(|row| row.start <= cursor).unwrap_or(0);
    let skipped = all.len().saturating_sub(shown).min(cursor_row);
    let end = (skipped + shown).min(all.len());
    let rows = all[skipped..end]
        .iter()
        .map(|row| text[row.clone()].to_string())
        .collect();
    let column = text[all[cursor_row].start..cursor].chars().count();
    let cursor = (
        (PROMPT_LEAD.len() + column) as u16,
        (cursor_row - skipped) as u16,
    );
    InputView {
        rows,
        starts_here: skipped == 0,
        cursor,
    }
}

/// Break `text` into rows of at most `width` characters at word breaks, as
/// byte ranges of `text`. A word longer than a row is split, and the last
/// row is never full, so the cursor always has a column after the last
/// character.
fn wrap_rows(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for token in chars.chunk_by(|a, b| (a.1 == ' ') == (b.1 == ' ')) {
        let is_space = token[0].1 == ' ';
        if used + token.len() > width && !is_space && used > 0 {
            rows.push(start..token[0].0);
            start = token[0].0;
            used = 0;
        }
        for &(i, _) in token {
            if used == width {
                rows.push(start..i);
                start = i;
                used = 0;
            }
            used += 1;
        }
    }
    rows.push(start..text.len());
    if used == width {
        rows.push(text.len()..text.len());
    }
    rows
}

#[cfg(test)]
fn wrap_input(text: &str, width: usize) -> Vec<String> {
    wrap_rows(text, width)
        .into_iter()
        .map(|row| text[row].to_string())
        .collect()
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
    // the box sits on the status line, which has the chips it hangs from
    let height = (visible + error) as u16 + 2;
    let area = Rect {
        x: menu.anchor.min(screen.width - width),
        y: screen.height.saturating_sub(1 + height),
        width,
        height,
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
                Part::Flow(rows) | Part::Block { rows, .. } => rows.clone(),
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

    fn rows(tool: &Tool, width: usize) -> Vec<String> {
        tool_block(tool, Path::new("/work"), width)
            .into_iter()
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn a_tool_title_too_long_for_the_row_is_cut_with_an_ellipsis() {
        let tool = tool("Read a/very/long/path/to/some/file.rs", "/work/x.rs", None);
        let rows = rows(&tool, 30);
        assert_eq!(rows[0], "● read   Read a/very/long/pat…");
        assert_eq!(rows[0].chars().count(), 30);
        assert_eq!(rows[1], "  ⎿  x.rs");
    }

    #[test]
    fn the_location_is_dropped_when_the_title_already_names_the_file() {
        let shown = tool("Read src/main.rs", "/work/src/main.rs", Some(3));
        assert_eq!(rows(&shown, 80), ["● read   Read src/main.rs"]);
        let other = tool("Read the entry point", "/work/src/main.rs", Some(3));
        assert_eq!(rows(&other, 80)[1], "  ⎿  src/main.rs:3");
    }

    #[test]
    fn the_location_gives_way_when_the_row_cannot_hold_it() {
        let tool = tool("Read the entry point", "/work/src/main.rs", Some(3));
        assert_eq!(rows(&tool, 15), ["● read   Read …"]);
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
        let view = input_view("hi   ", 5, 20, 10);
        assert_eq!(view.cursor, (2 + 5, 0));
    }

    #[test]
    fn a_row_filled_to_the_edge_puts_the_cursor_on_a_new_row() {
        let view = input_view("abcd", 4, 4, 10);
        assert_eq!(view.rows, ["abcd", ""]);
        assert_eq!(view.cursor, (2, 1));
    }

    #[test]
    fn the_cursor_sits_where_the_text_is_edited_across_wrapped_rows() {
        let view = input_view("one two three", 4, 9, 10);
        assert_eq!(view.rows, ["one two ", "three"]);
        assert_eq!(view.cursor, (2 + 4, 0));
        let view = input_view("one two three", 8, 9, 10);
        assert_eq!(view.cursor, (2, 1));
        let view = input_view("one two three", 0, 9, 10);
        assert_eq!(view.cursor, (2, 0));
    }

    #[test]
    fn the_window_follows_a_cursor_above_the_last_rows() {
        let text = "a ".repeat(30);
        let view = input_view(&text, 0, 4, 5);
        assert!(view.starts_here);
        assert_eq!(view.cursor, (2, 0));
    }

    #[test]
    fn the_prompt_shows_one_row_for_short_text_and_grows_with_long_text() {
        assert_eq!(input_view("", 0, 20, 10).height(), 1);
        let long = "word ".repeat(12);
        assert_eq!(input_view(&long, long.len(), 20, 10).height(), 4);
    }

    #[test]
    fn past_the_limit_the_prompt_shows_the_last_rows() {
        let text = "a ".repeat(30);
        let view = input_view(&text, text.len(), 4, 5);
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
        let [Part::Block { natural, rows, .. }] = &parts[..] else {
            panic!("expected one block");
        };
        assert_eq!(*natural, 14);
        assert_eq!(plain(rows), ["┌ rs", "│ abcdefghij", "│ abc"]);
    }

    #[test]
    fn a_table_is_as_wide_as_its_longest_row_with_the_padding_trimmed() {
        let text = "| name | note |\n|---|---|\n| a | a rather long note indeed |";
        let parts = markdown(text, 12);
        let [Part::Block { natural, rows, .. }] = &parts[..] else {
            panic!("expected one block");
        };
        let widest = rows.iter().map(Line::width).max().unwrap();
        assert_eq!(*natural, widest);
        assert!(widest > 12 && widest < 40, "{widest}");
    }

    #[test]
    fn a_table_that_fits_the_width_is_laid_out_with_the_prose() {
        let parts = markdown("| a | b |\n|---|---|\n| 1 | 2 |\n\nafter", 20);
        assert!(matches!(&parts[..], [Part::Flow(_)]));
    }

    #[test]
    fn every_code_block_is_a_block_of_its_own_with_its_text_to_copy() {
        let parts = markdown(
            "before\n\n```\nfn main() {\n    run();\n}\n```\n\nafter",
            40,
        );
        let [Part::Flow(_), Part::Block { copy, .. }, Part::Flow(_)] = &parts[..] else {
            panic!("expected prose, a block and prose");
        };
        assert_eq!(copy.as_deref(), Some("fn main() {\n    run();\n}"));
    }

    #[test]
    fn a_wide_code_block_copies_its_whole_text_not_the_window() {
        let long = "x".repeat(60);
        let mut rendered = Rendered::default();
        let text = format!("```rs\n{long}\nshort\n```");
        rendered.agent(3, &text, 20);
        assert_eq!(
            rendered.copy_text((3, 0)),
            Some(format!("{long}\nshort")).as_deref()
        );
        assert_eq!(rendered.copy_text((3, 1)), None);
        assert_eq!(rendered.copy_text((4, 0)), None);
    }

    #[test]
    fn a_table_has_no_text_to_copy() {
        let text = "| name | note |\n|---|---|\n| a | a rather long note indeed |";
        let mut rendered = Rendered::default();
        rendered.agent(0, text, 12);
        assert_eq!(rendered.copy_text((0, 0)), None);
    }

    #[test]
    fn the_copy_glyph_takes_the_last_column_of_the_first_row() {
        let row = Line::from("0123456789");
        let at = |offset| window_with_corner(&row, offset, 6, "#", Style::default()).to_string();
        assert_eq!(at(0), "01234#");
        assert_eq!(at(4), "45678#");
    }

    #[test]
    fn a_window_shows_the_columns_from_the_offset_with_no_edge_markers() {
        let row = Line::from("0123456789");
        assert_eq!(window(&row, 0, 5).to_string(), "01234");
        assert_eq!(window(&row, 5, 5).to_string(), "56789");
    }

    #[test]
    fn a_window_pads_a_row_shorter_than_the_block() {
        let row = Line::from("01");
        assert_eq!(window(&row, 0, 5).to_string(), "01   ");
        assert_eq!(window(&row, 3, 5).to_string(), "     ");
    }

    #[test]
    fn the_thumb_is_the_visible_share_of_the_block_and_moves_with_the_pan() {
        // half the block is visible
        assert_eq!(thumb(20, 20, 0, 2), (0, 10));
        assert_eq!(thumb(20, 20, 10, 2), (5, 10));
        assert_eq!(thumb(20, 20, 20, 2), (10, 10));
        // a tenth is visible, so the thumb stays at its minimum of 2
        assert_eq!(thumb(20, 180, 0, 2), (0, 2));
        assert_eq!(thumb(20, 180, 90, 2), (9, 2));
        assert_eq!(thumb(20, 180, 180, 2), (18, 2));
        // nearly all of it is visible
        assert_eq!(thumb(20, 2, 0, 2), (0, 18));
        assert_eq!(thumb(20, 2, 2, 2), (2, 18));
    }

    #[test]
    fn the_thumb_never_leaves_a_one_column_bar() {
        assert_eq!(thumb(1, 5, 3, 2), (0, 1));
        assert_eq!(thumb(2, 5, 5, 2), (0, 2));
    }

    #[test]
    fn the_pan_for_a_thumb_position_inverts_the_thumb_and_stops_at_the_ends() {
        for offset in 0..=4 {
            let (start, _) = thumb(40, 4, offset, 2);
            assert_eq!(pan_for_thumb(40, 4, start as isize, 2), offset);
        }
        assert_eq!(pan_for_thumb(20, 20, 5, 2), 10);
        assert_eq!(pan_for_thumb(20, 20, -7, 2), 0);
        assert_eq!(pan_for_thumb(20, 20, 99, 2), 20);
    }

    #[test]
    fn the_scroll_bar_row_is_a_dim_track_with_a_thumb_of_the_foreground_colour() {
        let bar = scroll_bar(10, 10, 5, false);
        assert_eq!(bar.to_string(), "───━━━━━──");
        assert_eq!(bar.width(), 10);
        assert_eq!(bar.spans[1].style.fg, Some(palette::get().foreground));
        assert_eq!(bar.spans[0].style, dim());
        let hot = scroll_bar(10, 10, 5, true);
        assert_eq!(hot.spans[1].style.fg, Some(palette::get().accent));
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
            assert_eq!(flat(&markdown(text, width)), whole);
        }
    }

    fn diff_of(old: Option<&str>, new: &str) -> Vec<ToolCallContent> {
        let mut diff = Diff::new("src/main.rs", new);
        diff.old_text = old.map(str::to_string);
        vec![ToolCallContent::Diff(diff)]
    }

    #[test]
    fn a_line_diff_marks_removed_added_and_unchanged_lines() {
        assert_eq!(
            line_diff("a\nb\nc", "a\nx\nc"),
            [(' ', "a"), ('-', "b"), ('+', "x"), (' ', "c")]
        );
        assert_eq!(
            line_diff("a\nb", "a\nb\nc"),
            [(' ', "a"), (' ', "b"), ('+', "c")]
        );
        assert_eq!(line_diff("", "a"), [('+', "a")]);
        assert_eq!(line_diff("a\nb", ""), [('-', "a"), ('-', "b")]);
    }

    #[test]
    fn a_line_diff_keeps_the_lines_two_ends_share_when_the_middle_moved() {
        assert_eq!(
            line_diff("a\nb\nc\nd\ne", "a\nc\nb\nd\ne"),
            [
                (' ', "a"),
                ('-', "b"),
                (' ', "c"),
                ('+', "b"),
                (' ', "d"),
                (' ', "e")
            ]
        );
    }

    #[test]
    fn diff_rows_show_removed_lines_red_and_added_lines_green() {
        let rows = diff_rows(&diff_of(Some("a\nb\nc"), "a\nx\nc"), 40);
        assert_eq!(plain(&rows), ["  a", "- b", "+ x", "  c"]);
        assert_eq!(rows[1].style.fg, Some(palette::get().red));
        assert_eq!(rows[2].style.fg, Some(palette::get().green));
    }

    #[test]
    fn a_new_file_shows_every_line_added() {
        let rows = diff_rows(&diff_of(None, "one\ntwo\n"), 40);
        assert_eq!(plain(&rows), ["+ one", "+ two"]);
    }

    #[test]
    fn a_long_run_of_unchanged_lines_folds_into_a_count_but_a_lone_line_stays() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9";
        let rows = diff_rows(&diff_of(Some(old), "x\n2\n3\n4\n5\n6\n7\n8\ny"), 40);
        assert_eq!(
            plain(&rows),
            [
                "- 1",
                "+ x",
                "  2",
                "  3",
                "⋯ 3 unchanged",
                "  7",
                "  8",
                "- 9",
                "+ y"
            ]
        );
        let rows = diff_rows(&diff_of(Some("1\n2\n3\n4\n5\n6"), "x\n2\n3\n4\n5\ny"), 40);
        assert_eq!(
            plain(&rows),
            ["- 1", "+ x", "  2", "  3", "  4", "  5", "- 6", "+ y"]
        );
    }

    #[test]
    fn a_diff_line_is_cut_to_the_width() {
        let rows = diff_rows(&diff_of(None, "abcdefghij"), 6);
        assert_eq!(plain(&rows), ["+ abc…"]);
    }

    #[test]
    fn an_expanded_edit_shows_its_diff_indented_under_its_rows() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let cwd = Path::new("/work");
        let (folded, head) = tool_view(&edit, cwd, 60, false, false);
        assert_eq!((folded.len(), head), (2, 2));
        let (open, head) = tool_view(&edit, cwd, 60, true, false);
        assert_eq!(head, 2);
        assert_eq!(&plain(&open)[2..], ["    - a", "    + b"]);
    }

    fn text_content(text: &str) -> Vec<ToolCallContent> {
        vec![ToolCallContent::from(text)]
    }

    #[test]
    fn an_expanded_run_shows_its_output_through_the_markdown_path() {
        let mut run = tool("cargo run", "/work/x", None);
        run.kind = ToolKind::Execute;
        run.locations.clear();
        run.content = text_content("```console\n$ cargo run\nhello\n```");
        let (open, head) = tool_view(&run, Path::new("/work"), 60, true, false);
        assert_eq!(head, 1);
        assert_eq!(
            &plain(&open)[1..],
            ["    ┌ console", "    │ $ cargo run", "    │ hello"]
        );
    }

    fn last_cell(line: &Line) -> (String, Style) {
        let span = line.spans.last().unwrap();
        (span.content.chars().last().unwrap().to_string(), span.style)
    }

    #[test]
    fn a_folded_expandable_call_ends_its_title_row_with_a_dim_right_marker() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let (rows, _) = tool_view(&edit, Path::new("/work"), 40, false, false);
        assert_eq!(rows[0].width(), 40);
        assert_eq!(last_cell(&rows[0]), ("▸".to_string(), dim()));
    }

    #[test]
    fn an_expanded_call_shows_the_down_marker_and_a_hovered_one_the_accent() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let (rows, _) = tool_view(&edit, Path::new("/work"), 40, true, true);
        let (glyph, style) = last_cell(&rows[0]);
        assert_eq!(glyph, "▾");
        assert_eq!(style.fg, Some(palette::get().accent));
    }

    #[test]
    fn a_subagent_call_has_the_marker_and_a_call_with_nothing_to_expand_has_none() {
        let (rows, head) = tool_view(&task(), Path::new("/work"), 60, false, false);
        assert_eq!(head, 2);
        assert!(rows[0].to_string().ends_with('▸'));
        assert_eq!(rows[0].width(), 60);
        let read = tool("Read src/main.rs", "/work/src/main.rs", None);
        let (rows, _) = tool_view(&read, Path::new("/work"), 60, false, false);
        assert!(!rows[0].to_string().ends_with(['▸', '▾']));
    }

    #[test]
    fn a_long_title_is_cut_before_the_marker() {
        let mut edit = tool("Edit a/very/long/path/to/some/file.rs", "/work/x", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let (rows, _) = tool_view(&edit, Path::new("/work"), 24, false, false);
        assert_eq!(rows[0].width(), 24);
        assert!(rows[0].to_string().contains('…'));
        assert!(rows[0].to_string().ends_with('▸'));
    }

    #[test]
    fn only_an_edit_with_a_diff_or_a_run_with_output_expands() {
        let mut edit = tool("Edit", "/work/x", None);
        edit.kind = ToolKind::Edit;
        assert!(!expandable(&edit));
        edit.content = diff_of(None, "a");
        assert!(expandable(&edit));

        let mut run = tool("run", "/work/x", None);
        run.kind = ToolKind::Execute;
        assert!(!expandable(&run));
        run.content = text_content("  \n");
        assert!(!expandable(&run));
        run.content = text_content("out");
        assert!(expandable(&run));

        let mut read = tool("Read", "/work/x", None);
        read.content = text_content("file text");
        assert!(!expandable(&read));
        assert!(expandable(&task()));
    }

    #[test]
    fn a_done_call_with_nothing_to_add_has_no_status_row() {
        let mut quiet = tool("Read the entry point", "/work/x", None);
        quiet.locations.clear();
        assert_eq!(rows(&quiet, 80), ["● read   Read the entry point"]);
    }

    #[test]
    fn a_call_that_is_not_done_keeps_its_status_row() {
        let mut call = tool("Read the entry point", "/work/x", None);
        call.locations.clear();
        for (status, word) in [
            (ToolCallStatus::InProgress, "running…"),
            (ToolCallStatus::Failed, "failed"),
            (ToolCallStatus::Pending, "pending"),
        ] {
            call.status = status;
            assert_eq!(rows(&call, 80)[1], format!("  ⎿  {word}"));
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
