use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::{
    ContentBlock, PlanEntryStatus, ToolCallContent, ToolCallId, ToolCallStatus, ToolKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, ExitKey, MenuTarget};
use crate::clock::Time;
use crate::command;
use crate::diff::line_diff;
use crate::form::{self, Form, Kind as FormKind, Row};
use crate::header;
use crate::links::{self, Kind};
use crate::palette;
use crate::prompt;
use crate::select::{self, LineText};
use crate::session::{Child, Entry, Message, Tool};
use crate::settings::{self, Control};

const MAX_PLAN_ROWS: usize = 6;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.buffer_mut().set_style(area, palette::base());
    app.prompt_width = (area.width as usize).saturating_sub(PROMPT_LEAD.len());
    let input = input_view(
        &app.input,
        app.cursor,
        app.prompt_width,
        (area.height / 2) as usize,
    );
    let plan_rows = if app.show_plan {
        app.transcript.plan.len().min(MAX_PLAN_ROWS)
    } else {
        0
    };
    let form = app
        .form
        .as_ref()
        .map(|asked| form_lines(&asked.form, app.prompt_width));
    let bottom_rows = match &form {
        Some((lines, _)) => lines.len().clamp(1, (area.height / 2) as usize),
        None => input.height(),
    };
    let working_rows = u16::from(app.transcript.in_flight);
    let waiting_rows = app.waiting.len().min(MAX_WAITING_ROWS) as u16;
    let [
        transcript_area,
        plan_area,
        waiting_area,
        working_area,
        upper_rule,
        prompt_area,
        lower_rule,
        context_area,
        status_area,
        progress_area,
        baseline,
    ] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(plan_rows as u16),
        Constraint::Length(waiting_rows),
        Constraint::Length(working_rows),
        Constraint::Length(1),
        Constraint::Length(bottom_rows as u16),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, transcript_area, app);
    draw_permission(frame, transcript_area, app);
    draw_plan(frame, plan_area, app);
    let waiting = waiting_lines(&app.waiting, waiting_area.width as usize);
    frame.render_widget(Paragraph::new(waiting), waiting_area);
    if app.transcript.in_flight {
        let elapsed = app
            .transcript
            .turn_started
            .map_or(Duration::ZERO, |start| start.elapsed());
        let waiting = app.permission.is_some() || app.form.is_some();
        let line = working_line(app.spinner, elapsed, waiting);
        frame.render_widget(Paragraph::new(line), working_area);
    }
    match form {
        Some((lines, focus)) => draw_form(frame, upper_rule, prompt_area, app, lines, focus),
        None => {
            draw_rule(frame, upper_rule, THIN);
            draw_input(frame, prompt_area, &input, prompt_colour(&app.input));
            draw_commands(frame, app, upper_rule);
        }
    }
    draw_confirm(frame, transcript_area, app);
    draw_guard_rule(frame, lower_rule, app);
    header::draw_context(frame, context_area, app);
    header::draw_status(frame, status_area, app);
    header::draw_progress(frame, progress_area, app);
    draw_rule(frame, baseline, THIN);
    draw_menu(frame, app, status_area.y, Time::now());
    draw_link_popup(frame, app);
}

const MAX_WAITING_ROWS: usize = 3;

/// One row per prompt waiting for its turn, each its first line, and a last
/// row counting the rest when they do not all fit.
fn waiting_lines(waiting: &VecDeque<String>, width: usize) -> Vec<Line<'static>> {
    let lead = " queued  ";
    let room = width.saturating_sub(lead.width() + 1);
    let shown = if waiting.len() > MAX_WAITING_ROWS {
        MAX_WAITING_ROWS - 1
    } else {
        waiting.len()
    };
    let accent = Style::new().fg(palette::get().accent);
    let foreground = Style::new().fg(palette::get().foreground);
    let mut lines: Vec<Line> = waiting
        .iter()
        .take(shown)
        .map(|text| {
            Line::from(vec![
                Span::raw(" "),
                Span::styled("queued", accent),
                Span::raw("  "),
                Span::styled(cut(first_line(text), room), foreground),
            ])
        })
        .collect();
    if waiting.len() > shown {
        lines.push(Line::from(vec![
            Span::styled(format!(" + {} more", waiting.len() - shown), foreground),
            Span::styled(" queued", accent),
        ]));
    }
    lines
}

/// The row above the prompt while a turn runs: the spinner, the time since
/// the prompt was sent and the cancel key, or that a permission waits.
fn working_line(spinner: usize, elapsed: Duration, waiting: bool) -> Line<'static> {
    let yellow = Style::new().fg(palette::get().yellow);
    let time = Span::styled(format!(" · {}", elapsed_text(elapsed)), dim());
    if waiting {
        return Line::from(vec![
            Span::raw(" "),
            Span::styled("waiting for you", yellow),
            time,
        ]);
    }
    let spinner = header::SPINNER[spinner % header::SPINNER.len()];
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{spinner} working"), yellow),
        time,
        Span::styled(" · esc to cancel", dim()),
    ])
}

pub fn elapsed_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    }
}

/// The popup over the link the pointer is on, below the pointer or above it
/// when there is no room.
fn draw_link_popup(frame: &mut Frame, app: &App) {
    let Some(hover) = &app.hover_link else {
        return;
    };
    let shown = links::popup(
        &hover.target,
        hover.title.as_deref(),
        &app.links,
        &app.cwd,
        &app.link_cache,
    );
    let Some(lines) = shown else {
        return;
    };
    let screen = frame.area();
    let inner = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0)
        .min(72)
        .min((screen.width as usize).saturating_sub(4));
    let width = (inner as u16 + 4).min(screen.width);
    let height = (lines.len() as u16 + 2).min(screen.height);
    let x = hover.at.x.min(screen.width - width);
    let y = if hover.at.y + 1 + height <= screen.height {
        hover.at.y + 1
    } else {
        hover.at.y.saturating_sub(height)
    };
    let area = Rect::new(x, y, width, height);
    let rows: Vec<Line> = lines
        .iter()
        .map(|line| Line::raw(cut(line, inner)))
        .collect();
    let block = Block::bordered()
        .border_style(dim())
        .padding(Padding::horizontal(1));
    clear(frame, area);
    frame.render_widget(Paragraph::new(rows).block(block), area);
}

/// A screen that says only `message`, centred and dim, while a session opens.
pub fn draw_waiting(frame: &mut Frame, message: &str) {
    let area = frame.area();
    let row = Rect::new(
        area.x,
        area.y + area.height / 2,
        area.width,
        1.min(area.height),
    );
    frame.render_widget(
        Paragraph::new(cut(message, area.width as usize))
            .centered()
            .style(dim()),
        row,
    );
}

/// The rule under the prompt, carrying the exit guard's message while armed.
fn draw_guard_rule(frame: &mut Frame, area: Rect, app: &App) {
    let Some(guard) = app.guard else {
        return match &app.unknown {
            Some(message) => draw_notice_rule(frame, area, message),
            None if app.exited => draw_notice_rule(frame, area, "the agent has exited"),
            None => draw_rule(frame, area, HEAVY),
        };
    };
    let message = match guard.key {
        ExitKey::CtrlC => "press ^c again to quit",
        ExitKey::CtrlD => "press ^d again to quit",
    };
    let rest = (area.width as usize).saturating_sub(message.width() + 4);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{HEAVY}{HEAVY} "), dim()),
            Span::styled(message, Style::new().fg(palette::get().yellow)),
            Span::styled(format!(" {}", HEAVY.repeat(rest)), dim()),
        ])),
        area,
    );
}

/// The rule under the prompt, carrying a dim `message`.
fn draw_notice_rule(frame: &mut Frame, area: Rect, message: &str) {
    let rest = (area.width as usize).saturating_sub(message.width() + 4);
    frame.render_widget(
        Paragraph::new(format!("{HEAVY}{HEAVY} {message} {}", HEAVY.repeat(rest))).style(dim()),
        area,
    );
}

/// The rule over the prompt and the footer's baseline.
const THIN: &str = "─";

/// The rule under the prompt, over the footer.
const HEAVY: &str = "━";

fn draw_rule(frame: &mut Frame, area: Rect, line: &str) {
    frame.render_widget(
        Paragraph::new(line.repeat(area.width as usize)).style(dim()),
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
    /// The rows under an expanded edit or command call, by call.
    expanded: HashMap<ToolCallId, ExpandedRows>,

    /// Each entry's layouts at the two widths the transcript measures a
    /// frame at, the latest first.
    entries: Vec<[Option<Markdown>; 2]>,
}

struct ExpandedRows {
    width: usize,
    revision: u32,
    rows: Drawn,
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
    Flow {
        rows: Drawn,

        /// Where the links of the rows are drawn.
        runs: Vec<Run>,
    },
    Block(KeptBlock),

    /// Adjacent code blocks that each name a tab, drawn as one box that
    /// shows one of them.
    Tabs {
        /// Which block of the entry the first tab is.
        first: usize,
        names: Vec<String>,
        tabs: Vec<KeptBlock>,
    },
}

/// A code block, or a table wider than the layout width, kept at its
/// natural width.
struct KeptBlock {
    /// Which block of the entry this is.
    block: usize,

    /// The rows at the block's natural width, trailing spaces trimmed.
    rows: Drawn,
    natural: usize,

    /// The text a copy takes, for a literal block.
    copy: Option<String>,

    /// Where the links of the rows are drawn, before the block pans.
    runs: Vec<Run>,

    /// A `markdown` block drawn as markdown.
    rendered: Option<MarkdownView>,

    /// A code block wider than the layout width, wrapped to it.
    wrapped: Option<Wrapped>,
}

/// The rows of a code block wrapped to the layout width, and whether the
/// block wraps until a click on its header row says otherwise.
struct Wrapped {
    rows: Drawn,
    natural: usize,
    default: bool,
}

/// The rows of a `markdown` block drawn as markdown, and the width of its
/// box, no wider than the layout width.
struct MarkdownView {
    rows: Drawn,
    natural: usize,

    /// Where the links of the rows are drawn, numbered among the links of
    /// the block alone.
    runs: Vec<Run>,
}

/// A word on a block's header row that switches how the block shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Word {
    Render,
    Raw,
    Wrap,
    Pan,

    /// The name of a tab, by its place in the box.
    Tab(usize),

    /// The `+N` after the tab names that fit, with the tab a click shows.
    More(usize),
}

/// The words of one choice on a block's header row, each with its text, and
/// the one in force.
struct Toggle<'a> {
    words: &'a [(Word, &'a str)],
    active: Word,
}

/// Columns between the language and the first toggle, and between toggles.
const TOGGLE_GAP: usize = 3;

/// The columns `toggles` take on a header row, the gap before each included.
fn toggles_width(toggles: &[Toggle]) -> usize {
    toggles
        .iter()
        .map(|toggle| {
            let words: usize = toggle.words.iter().map(|(_, text)| text.width()).sum();
            TOGGLE_GAP + words + toggle.words.len() - 1
        })
        .sum()
}

/// Draw `toggles` into a header row's `cells` from column `col`, dim with
/// the active word in the accent colour, and give each word's column, width
/// and word.
fn put_toggles(
    cells: &mut [(char, Style)],
    mut col: usize,
    toggles: &[Toggle],
) -> Vec<(usize, usize, Word)> {
    let mut spots = Vec::new();
    for toggle in toggles {
        col += TOGGLE_GAP;
        for (n, &(word, text)) in toggle.words.iter().enumerate() {
            if n > 0 {
                col += 1;
            }
            let style = if word == toggle.active {
                Style::new().fg(palette::get().accent)
            } else {
                dim()
            };
            for (k, ch) in text.chars().enumerate() {
                if let Some(cell) = cells.get_mut(col + k) {
                    *cell = (ch, style);
                }
            }
            spots.push((col, text.width(), word));
            col += text.width();
        }
    }
    spots
}

const RENDER_RAW: &[(Word, &str)] = &[(Word::Render, "render"), (Word::Raw, "raw")];

const WRAP_PAN: &[(Word, &str)] = &[(Word::Wrap, "wrap"), (Word::Pan, "pan")];

/// The tabs whose names fit in `room` columns of a header row, the gap
/// before them and a `+N` for the rest included. The names that fit come
/// first, and the tab `shown` is among them when it fits at all.
fn fitting_tabs(names: &[String], shown: usize, room: usize) -> Vec<usize> {
    let width = |set: &[usize]| {
        let mut words: Vec<usize> = set.iter().map(|&n| names[n].width()).collect();
        let hidden = names.len() - set.len();
        if hidden > 0 {
            words.push(format!("+{hidden}").len());
        }
        TOGGLE_GAP + words.iter().sum::<usize>() + words.len().saturating_sub(1)
    };
    (0..=names.len())
        .rev()
        .map(|k| {
            let mut set: Vec<usize> = (0..k).collect();
            if shown >= k {
                set.push(shown);
            }
            set
        })
        .find(|set| width(set) <= room)
        .unwrap_or_default()
}

/// A `markdown` block's `source` drawn as markdown under its `head` row, in
/// a box no wider than `width`. `header` is the width the head row takes
/// after its gutter.
fn markdown_view(head: &Line<'static>, source: &str, header: usize, width: usize) -> MarkdownView {
    let mut blocks = cmr::parse(source);
    wrap_code(&mut blocks);
    let links = cmr::links(&blocks);
    let gutter = head
        .spans
        .first()
        .map_or(Style::default(), |span| span.style);
    let inner = width.saturating_sub(2 + COPY_ROOM).max(1);
    let laid = cmr::layout_marked(&blocks, inner);

    // the head row comes first, and `│ ` starts every row after it
    let runs = link_runs(&laid, &links)
        .into_iter()
        .map(|run| Run {
            row: run.row + 1,
            col: run.col + 2,
            ..run
        })
        .collect();
    let mut rows = Drawn::default();
    rows.push(head.clone(), None);
    for (row, mark) in laid {
        let mut spans = vec![Span::styled("│ ", gutter)];
        spans.extend(
            row.into_iter()
                .map(|(text, style)| Span::styled(text, link_style(style, &links))),
        );
        rows.push_text(trim_end(Line::from(spans)), 2 + mark.lead, mark.join);
    }
    let widest = rows.lines[1..].iter().map(Line::width).max().unwrap_or(0);
    let natural = widest.max(header + 2) + COPY_ROOM;
    MarkdownView {
        rows,
        natural: natural.min(width),
        runs,
    }
}

/// Set every code block in `blocks`, quoted ones included, to wrap.
fn wrap_code(blocks: &mut [cmr::Block]) {
    for block in blocks {
        match block {
            cmr::Block::Code { wrap, .. } => *wrap = true,
            cmr::Block::Quote(inner) => wrap_code(inner),
            _ => {}
        }
    }
}

/// Whether a code block fenced with `lang` wraps by default: one with no
/// language, `text`, or markdown does, and one in a programming language pans.
fn wraps_by_default(lang: Option<&str>) -> bool {
    lang.is_none_or(|lang| lang.eq_ignore_ascii_case("text") || is_markdown(lang))
}

/// Whether a fence's language names markdown.
fn is_markdown(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("markdown") || lang.eq_ignore_ascii_case("md")
}

/// A link's text on one row of a part, by column.
struct Run {
    row: usize,
    col: usize,
    width: usize,

    /// The link's place among the links of the whole entry, or of a
    /// rendered markdown block for a run in its `MarkdownView`.
    index: usize,
    link: cmr::Link,
}

/// Columns a code block keeps free after its longest line, where the copy
/// glyph sits on the first row.
const COPY_ROOM: usize = 2;

/// The copy glyph, one column wide in every font.
const COPY_GLYPH: &str = "\u{29c9}";

/// The widest a table is laid out when measuring it.
const TABLE_WIDTH: usize = 1000;

impl Rendered {
    /// The text a copy of block `key` (an entry and a block within it) takes.
    pub fn copy_text(&self, key: (usize, usize)) -> Option<&str> {
        let markdown = self.entries.get(key.0)?.iter().flatten().next()?;
        markdown
            .parts
            .iter()
            .flat_map(|part| match part {
                Part::Flow { .. } => &[],
                Part::Block(kept) => std::slice::from_ref(kept),
                Part::Tabs { tabs, .. } => tabs.as_slice(),
            })
            .find(|kept| kept.block == key.1)?
            .copy
            .as_deref()
    }

    /// The diff or output rows under an expanded call, indented, laid out
    /// again only when the call's content or the width changes.
    fn expanded(&mut self, tool: &Tool, inner: usize) -> &Drawn {
        let fresh = matches!(
            self.expanded.get(&tool.id),
            Some(done) if done.width == inner && done.revision == tool.revision
        );
        if !fresh {
            let mut rows = match tool.kind {
                ToolKind::Edit => diff_rows(&tool.content, inner),
                _ => output_rows(&tool.content, inner),
            };
            for row in &mut rows.lines {
                row.spans.insert(0, Span::raw(SUBAGENT_INDENT));
            }
            rows.indent(SUBAGENT_INDENT.len());
            let done = ExpandedRows {
                width: inner,
                revision: tool.revision,
                rows,
            };
            self.expanded.insert(tool.id.clone(), done);
        }
        &self.expanded[&tool.id].rows
    }

    fn agent(&mut self, index: usize, text: &str, width: usize) -> &[Part] {
        if self.entries.len() <= index {
            self.entries.resize_with(index + 1, Default::default);
        }
        let pair = &mut self.entries[index];
        let at = pair
            .iter()
            .position(|slot| matches!(slot, Some(done) if done.width == width))
            .unwrap_or(1);
        let fresh =
            matches!(&pair[at], Some(done) if done.width == width && done.len == text.len());
        if !fresh {
            pair[at] = Some(Markdown {
                width,
                len: text.len(),
                parts: markdown(text, width),
            });
        }
        pair.swap(0, at);
        pair[0].as_ref().map_or(&[], |done| &done.parts)
    }
}

/// `text` laid out one block at a time. Prose goes at `width`. A code block
/// keeps its natural width, as does a table wider than `width`. Either is a
/// `Part::Block`, and the screen windows one wider than `width`. Adjacent
/// code blocks that each name a tab are one `Part::Tabs`.
fn markdown(text: &str, width: usize) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    let blocks = cmr::parse(text);

    // a link's index counts across the entry's blocks, and a block laid out
    // alone keeps the index the whole parse gave it
    let links = cmr::links(&blocks);
    let mut block = 0;
    while block < blocks.len() {
        if block > 0 {
            flow(&mut parts)
                .0
                .push_text(Line::default(), 0, cmr::Join::Break);
        }
        let names: Vec<String> = blocks[block..]
            .iter()
            .map_while(|item| match item {
                cmr::Block::Code {
                    tab: Some(name), ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect();
        if !names.is_empty() {
            let end = block + names.len();
            let tabs = (block..end)
                .filter_map(|n| kept_whole(n, &blocks[n], width, &links))
                .collect();
            parts.push(Part::Tabs {
                first: block,
                names,
                tabs,
            });
            block = end;
            continue;
        }
        let item = &blocks[block];
        match kept_whole(block, item, width, &links) {
            Some(kept) => parts.push(Part::Block(kept)),
            None => {
                let (rows, runs) = block_rows(item, width, &links);
                let (flow_rows, flow_runs) = flow(&mut parts);
                let base = flow_rows.len();
                flow_runs.extend(runs.into_iter().map(|run| Run {
                    row: run.row + base,
                    ..run
                }));
                flow_rows.append(rows);
            }
        }
        block += 1;
    }
    parts
}

fn flow(parts: &mut Vec<Part>) -> (&mut Drawn, &mut Vec<Run>) {
    if !matches!(parts.last(), Some(Part::Flow { .. })) {
        parts.push(Part::Flow {
            rows: Drawn::default(),
            runs: Vec::new(),
        });
    }
    match parts.last_mut() {
        Some(Part::Flow { rows, runs }) => (rows, runs),
        _ => unreachable!(),
    }
}

/// The rows of one block, and where the links that act on a click sit in
/// them. `links` are the links of the whole entry.
fn block_rows(block: &cmr::Block, width: usize, links: &[cmr::Link]) -> (Drawn, Vec<Run>) {
    let rows = cmr::layout_marked(std::slice::from_ref(block), width);
    let runs = link_runs(&rows, links);
    let mut lines = Drawn::default();
    for (row, mark) in rows {
        let line = row
            .into_iter()
            .map(|(text, style)| Span::styled(text, link_style(style, links)))
            .collect();
        lines.push_text(trim_end(line), mark.lead, mark.join);
    }
    (lines, runs)
}

/// Where the links that act on a click sit in laid out `rows`, whose link
/// numbers index `links`.
fn link_runs(rows: &[(cmr::Row, cmr::Mark)], links: &[cmr::Link]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (row, (pieces, _)) in rows.iter().enumerate() {
        let mut col = 0;
        for (text, style) in pieces {
            let piece = text.width();
            if let Some(index) = style.link
                && let Some(link) = links.get(index)
                && !matches!(links::classify(&link.target), Kind::Color(_))
            {
                match runs.last_mut() {
                    Some(last)
                        if last.row == row
                            && last.index == index
                            && last.col + last.width == col =>
                    {
                        last.width += piece
                    }
                    _ => runs.push(Run {
                        row,
                        col,
                        width: piece,
                        index,
                        link: link.clone(),
                    }),
                }
            }
            col += piece;
        }
    }
    runs
}

/// A code block, or a table wider than `width`, as block number `index`
/// kept whole.
fn kept_whole(
    index: usize,
    block: &cmr::Block,
    width: usize,
    links: &[cmr::Link],
) -> Option<KeptBlock> {
    match block {
        cmr::Block::Code { lang, lines, .. } => {
            let markdown = lang.as_deref().is_some_and(is_markdown);
            let longest = lines.iter().map(|line| line.width()).max().unwrap_or(0);

            // the gutter is two columns: `│ ` on a line, `┌ ` before the language
            let wide = longest + 2 + COPY_ROOM > width;
            let mut toggles = Vec::new();
            if markdown {
                toggles.push(Toggle {
                    words: RENDER_RAW,
                    active: Word::Render,
                });
            }
            if wide {
                toggles.push(Toggle {
                    words: WRAP_PAN,
                    active: Word::Wrap,
                });
            }

            // a wide block with no language gets an empty one, so its
            // header row has room for the toggle
            let headed = |wrap| cmr::Block::Code {
                lang: Some(lang.clone().unwrap_or_default()),
                lines: lines.clone(),
                wrap,
                tab: None,
            };
            let header = |toggles: &[Toggle]| {
                let gap = if toggles.is_empty() {
                    0
                } else {
                    TOGGLE_GAP - 1
                };
                match lang {
                    Some(lang) => lang.width() + toggles_width(toggles) + gap,
                    None if wide => toggles_width(toggles) + gap,
                    None => 0,
                }
            };
            let natural = longest.max(header(&toggles)) + 2 + COPY_ROOM;
            let (mut rows, runs) = if wide && lang.is_none() {
                block_rows(&headed(false), natural, links)
            } else {
                block_rows(block, natural, links)
            };

            // the header row holds the language and the toggles, which a
            // copy leaves out
            if lang.is_some() || wide {
                rows.texts[0] = None;
            }
            let source = lines.join("\n");
            let rendered = markdown
                .then(|| markdown_view(&rows.lines[0], &source, header(&toggles[..1]), width));
            let wrapped = wide.then(|| {
                let mut rows = block_rows(&headed(true), width.saturating_sub(COPY_ROOM), links).0;
                rows.texts[0] = None;
                Wrapped {
                    rows,
                    natural: width,
                    default: wraps_by_default(lang.as_deref()),
                }
            });
            Some(KeptBlock {
                block: index,
                rows,
                natural,
                copy: Some(source),
                runs,
                rendered,
                wrapped,
            })
        }
        cmr::Block::Table { .. } => {
            let (rows, runs) = block_rows(block, TABLE_WIDTH, links);
            let natural = rows.lines.iter().map(Line::width).max().unwrap_or(0);
            (natural > width).then_some(KeptBlock {
                block: index,
                rows,
                natural,
                copy: None,
                runs,
                rendered: None,
                wrapped: None,
            })
        }
        _ => None,
    }
}

/// The shown columns, as first and width, of a run in a block panned by
/// `offset` that shows `visible` columns, or `None` when none of it shows.
fn panned(col: usize, width: usize, offset: usize, visible: usize) -> Option<(usize, usize)> {
    let start = col.max(offset);
    let end = (col + width).min(offset + visible);
    (start < end).then(|| (start - offset, end - start))
}

/// `row` cut to the `visible` columns from `offset`.
fn window(row: &Line<'static>, offset: usize, visible: usize) -> Line<'static> {
    cells_line(window_cells(row, offset, visible))
}

/// The cell after a wide character's first, so a row has one cell per column.
const WIDE_TAIL: char = '\0';

fn window_cells(row: &Line<'static>, offset: usize, visible: usize) -> Vec<(char, Style)> {
    let mut columns: Vec<(char, Style)> = Vec::new();
    for span in &row.spans {
        for ch in span.content.chars() {
            match ch.width() {
                Some(1) => columns.push((ch, span.style)),
                Some(2) => columns.extend([(ch, span.style), (WIDE_TAIL, span.style)]),
                _ => {}
            }
        }
    }
    let mut cells: Vec<(char, Style)> = columns.into_iter().skip(offset).take(visible).collect();
    cells.resize(visible, (' ', Style::default()));

    // a wide character cut by either edge shows as a space
    if let Some(first) = cells.first_mut()
        && first.0 == WIDE_TAIL
    {
        first.0 = ' ';
    }
    blank_cut_head(&mut cells);
    cells
}

/// Replace the last cell with a space when it is a wide character's first
/// column, since its second column is outside the window.
fn blank_cut_head(cells: &mut [(char, Style)]) {
    if let Some(last) = cells.last_mut()
        && last.0.width() == Some(2)
    {
        last.0 = ' ';
    }
}

/// A block's header row: `window` with `toggles` after the row's own text
/// and `label` in the last columns, and where each toggle word was drawn.
fn header_row(
    row: &Line<'static>,
    offset: usize,
    visible: usize,
    toggles: &[Toggle],
    label: &str,
    style: Style,
) -> (Line<'static>, Vec<(usize, usize, Word)>) {
    let mut cells = window_cells(row, offset, visible);
    let mut spots = put_toggles(&mut cells, row.width().saturating_sub(offset), toggles);
    let label: Vec<char> = label.chars().take(visible).collect();
    let start = visible - label.len();
    for (n, ch) in label.iter().enumerate() {
        cells[start + n] = (*ch, style);
    }
    blank_cut_head(&mut cells[..start]);
    spots.retain(|(col, width, _)| col + width <= start);
    (cells_line(cells), spots)
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
        if ch == WIDE_TAIL {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), style)),
        }
    }
    Line::from(spans)
}

/// `screen_style`, with a link drawn as its target says: text of a
/// `color:` link in that palette colour and not underlined, and a `file:`
/// link in blue.
fn link_style(style: cmr::Style, links: &[cmr::Link]) -> Style {
    let out = screen_style(style);
    let Some(link) = style.link.and_then(|index| links.get(index)) else {
        return out;
    };
    match links::classify(&link.target) {
        Kind::Color(name) => {
            let out = out.remove_modifier(Modifier::UNDERLINED);
            match palette::get().named(name) {
                Some(colour) => out.fg(colour),
                None => out,
            }
        }
        Kind::File { .. } => out.fg(palette::get().blue),
        _ => out,
    }
}

/// `row` with `width` columns from `col` in `colour`.
fn paint(row: &mut Line<'static>, col: usize, width: usize, colour: Color) {
    let mut cells = window_cells(row, 0, row.width());
    for cell in cells.iter_mut().skip(col).take(width) {
        cell.1 = cell.1.fg(colour);
    }
    *row = cells_line(cells);
}

fn screen_style(style: cmr::Style) -> Style {
    let palette = palette::get();
    let mut out = Style::new();
    if let Some(color) = style.fg {
        out = out.fg(match color {
            cmr::Color::Black => palette.black,
            cmr::Color::Red => palette.red,
            cmr::Color::Green => palette.green,
            cmr::Color::Yellow => palette.yellow,
            cmr::Color::Blue => palette.blue,
            cmr::Color::Magenta => palette.magenta,
            cmr::Color::Cyan => palette.cyan,
            cmr::Color::Gray => palette.white,
            cmr::Color::DarkGray => palette.bright_black,
            cmr::Color::LightRed => palette.bright_red,
            cmr::Color::LightGreen => palette.bright_green,
            cmr::Color::LightYellow => palette.bright_yellow,
            cmr::Color::LightBlue => palette.bright_blue,
            cmr::Color::LightMagenta => palette.bright_magenta,
            cmr::Color::LightCyan => palette.bright_cyan,
            cmr::Color::White => palette.bright_white,
        });
    }
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        out = match (style.fg, palette.muted) {
            (None, Some(muted)) => out.fg(muted),
            _ => out.add_modifier(Modifier::DIM),
        };
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

/// Rows as drawn, each with the text it holds for a selection.
#[derive(Clone, Default)]
struct Drawn {
    lines: Vec<Line<'static>>,
    texts: Vec<Option<LineText>>,
}

impl Drawn {
    fn push(&mut self, line: Line<'static>, text: Option<LineText>) {
        self.lines.push(line);
        self.texts.push(text);
    }

    /// Push `line`, which holds text after its first `lead` columns.
    fn push_text(&mut self, line: Line<'static>, lead: usize, join: cmr::Join) {
        let text = line_text(&line, lead, join);
        self.push(line, Some(text));
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    fn append(&mut self, mut other: Drawn) {
        self.lines.append(&mut other.lines);
        self.texts.append(&mut other.texts);
    }

    fn extend(&mut self, other: &Drawn) {
        self.lines.extend_from_slice(&other.lines);
        self.texts.extend_from_slice(&other.texts);
    }

    /// Move the text of every row `by` columns right.
    fn indent(&mut self, by: usize) {
        for text in self.texts.iter_mut().flatten() {
            text.col += by;
        }
    }
}

/// The text of `line` after its first `lead` columns, without the spaces
/// that end it.
fn line_text(line: &Line, lead: usize, join: cmr::Join) -> LineText {
    let mut col = 0;
    let mut text = String::new();
    for ch in line.spans.iter().flat_map(|span| span.content.chars()) {
        if col >= lead {
            text.push(ch);
        }
        col += ch.width().unwrap_or(0);
    }
    text.truncate(text.trim_end().len());
    LineText {
        col: lead,
        text,
        join,
    }
}

/// `text` as a block panned by `offset` that shows `visible` columns shows
/// it: only the characters wholly inside the window.
fn windowed(text: &LineText, offset: usize, visible: usize) -> LineText {
    let mut col = text.col;
    let mut first = None;
    let mut kept = String::new();
    for ch in text.text.chars() {
        let width = ch.width().unwrap_or(0);
        if col >= offset && col + width <= offset + visible {
            first.get_or_insert(col);
            kept.push(ch);
        }
        col += width;
    }
    LineText {
        col: first.unwrap_or(offset) - offset,
        text: kept,
        join: text.join,
    }
}

/// The transcript laid out at one width.
struct Built {
    lines: Drawn,
    wides: Vec<Wide>,
    copies: Vec<Copy>,
    folds: Vec<Fold>,
    links: Vec<LinkSpot>,
    toggles: Vec<ToggleSpot>,
}

/// A toggle word drawn this frame, by its place among the transcript lines.
struct ToggleSpot {
    key: (usize, usize),
    line: usize,
    col: usize,
    width: usize,
    word: Word,
}

/// The rows a block shows, the width of its box, and whether it wraps when
/// it can. The markdown drawn shows when there is some and `raw` is off.
/// Otherwise the wrapped rows show when there are some and `wrap`, the
/// choice made on the block, or else the default, says to.
fn block_view(kept: &KeptBlock, raw: bool, wrap: Option<bool>) -> (&Drawn, usize, Option<bool>) {
    match (&kept.rendered, &kept.wrapped) {
        (Some(view), _) if !raw => (&view.rows, view.natural, None),
        (_, Some(wrapped)) if wrap.unwrap_or(wrapped.default) => {
            (&wrapped.rows, wrapped.natural, Some(true))
        }
        (_, Some(_)) => (&kept.rows, kept.natural, Some(false)),
        _ => (&kept.rows, kept.natural, None),
    }
}

/// The tab a box of `count` tabs shows: the one chosen on it, or the first.
fn shown_tab(tabs: &HashMap<(usize, usize), usize>, key: (usize, usize), count: usize) -> usize {
    tabs.get(&key).copied().unwrap_or(0).min(count - 1)
}

/// A link's text drawn this frame, by its place among the transcript lines.
struct LinkSpot {
    key: LinkKey,
    line: usize,
    col: usize,
    width: usize,
    link: cmr::Link,
}

/// The transcript laid out at `width`. Only the entries that reach the
/// lines in `visible` get their rows, and the others get blank lines of the
/// same height, since the screen never reads them.
fn build_transcript(app: &mut App, width: usize, visible: Range<usize>) -> Built {
    let usable = width - width / 10;
    let mut lines = Drawn::default();
    let mut wides: Vec<Wide> = Vec::new();
    let mut copies: Vec<Copy> = Vec::new();
    let mut folds: Vec<Fold> = Vec::new();
    let mut spots: Vec<LinkSpot> = Vec::new();
    let mut toggle_spots: Vec<ToggleSpot> = Vec::new();
    let hovered = app.hover_link.as_ref().map(|hover| hover.key);
    let copied = app
        .copied
        .as_ref()
        .filter(|(_, until)| *until > Instant::now())
        .map(|(key, _)| *key);
    let today = Time::now();
    for (index, entry) in app.transcript.entries.iter().enumerate() {
        match entry {
            Entry::User(prompt) => lines.append(bubble(prompt, today, width)),
            Entry::Agent(reply) => {
                let widest = (width * 9 / 10).max(5);
                let visible_width = widest - 4;
                let parts = app.rendered.agent(index, &reply.text, visible_width);

                // a frame's two border rows, then each part's rows and the
                // scroll bar row under a block that pans
                let height = 2 + parts
                    .iter()
                    .map(|part| {
                        let kept = match part {
                            Part::Flow { rows, .. } => return rows.len(),
                            Part::Block(kept) => kept,
                            Part::Tabs { first, tabs, .. } => {
                                &tabs[shown_tab(&app.tabs, (index, *first), tabs.len())]
                            }
                        };
                        let key = (index, kept.block);
                        let (rows, natural, _) =
                            block_view(kept, app.raw.contains(&key), app.wraps.get(&key).copied());
                        rows.len() + usize::from(natural > visible_width)
                    })
                    .sum::<usize>();
                let first = lines.len();
                if first >= visible.end || first + height <= visible.start {
                    for _ in 0..=height {
                        lines.push(Line::default(), None);
                    }
                    continue;
                }
                let mut rows = Drawn::default();
                for part in parts {
                    let (kept, tabbed) = match part {
                        Part::Flow { rows: flow, runs } => {
                            let start = rows.len();
                            let top = lines.len() + 1 + start;
                            rows.extend(flow);
                            for run in runs {
                                let key = (index, None, run.index);
                                if hovered == Some(key) {
                                    let row = &mut rows.lines[start + run.row];
                                    paint(row, run.col, run.width, palette::get().accent);
                                }
                                spots.push(LinkSpot {
                                    key,
                                    line: top + run.row,
                                    col: run.col,
                                    width: run.width,
                                    link: run.link.clone(),
                                });
                            }
                            continue;
                        }
                        Part::Block(kept) => (kept, None),
                        Part::Tabs { first, names, tabs } => {
                            let tab = shown_tab(&app.tabs, (index, *first), tabs.len());
                            (&tabs[tab], Some((*first, names, tab, tabs)))
                        }
                    };
                    let KeptBlock {
                        block,
                        copy,
                        runs,
                        rendered,
                        ..
                    } = kept;
                    let key = (index, *block);
                    let raw = app.raw.contains(&key);
                    let (block_rows, natural, wrap) =
                        block_view(kept, raw, app.wraps.get(&key).copied());
                    let (runs, numbered_in) = match rendered {
                        Some(view) if !raw => (&view.runs[..], Some(*block)),
                        _ if wrap == Some(true) => (&[][..], None),
                        _ => (&runs[..], None),
                    };
                    let more;
                    let short_name;
                    let tab_words: Vec<(Word, &str)>;
                    let mut toggles = Vec::new();
                    if rendered.is_some() {
                        toggles.push(Toggle {
                            words: RENDER_RAW,
                            active: if raw { Word::Raw } else { Word::Render },
                        });
                    }
                    if let Some(wrap) = wrap {
                        toggles.push(Toggle {
                            words: WRAP_PAN,
                            active: if wrap { Word::Wrap } else { Word::Pan },
                        });
                    }
                    let mut boxed = natural;
                    let box_key = (index, tabbed.map_or(*block, |tabbed| tabbed.0));
                    if let Some((_, names, tab, tabs)) = tabbed {
                        // the language before the tab names, and the
                        // other toggles and the copy glyph after them
                        let head = block_rows.lines[0].width();
                        let rest = |toggles: &[Toggle]| {
                            toggles_width(toggles) + TOGGLE_GAP - 1 + COPY_ROOM
                        };
                        let all: usize = names.iter().map(|name| name.width() + 1).sum();
                        boxed = tabs
                            .iter()
                            .map(|tab| {
                                let key = (index, tab.block);
                                let raw = app.raw.contains(&key);
                                block_view(tab, raw, app.wraps.get(&key).copied()).1
                            })
                            .fold(head + TOGGLE_GAP + all - 1 + rest(&toggles), usize::max);
                        let room = |toggles: &[Toggle]| {
                            boxed
                                .min(visible_width)
                                .saturating_sub(head + rest(toggles))
                        };

                        // the other toggles give up their room before
                        // the shown tab's name does
                        let mut fit = fitting_tabs(names, tab, room(&toggles));
                        while fit.is_empty() && toggles.pop().is_some() {
                            fit = fitting_tabs(names, tab, room(&toggles));
                        }
                        let mut words: Vec<(Word, &str)> = fit
                            .iter()
                            .map(|&n| (Word::Tab(n), names[n].as_str()))
                            .collect();
                        let mut counted = true;
                        if fit.is_empty() {
                            fit.push(tab);
                            let count = format!("+{}", names.len() - 1);
                            let more_room = if names.len() > 1 { count.len() + 1 } else { 0 };
                            let mut name_room =
                                room(&toggles).saturating_sub(TOGGLE_GAP + more_room);

                            // a lone `…` names no tab, so the count
                            // gives up its room before the name does
                            if name_room < 2 && names[tab].width() > name_room {
                                counted = false;
                                name_room = room(&toggles).saturating_sub(TOGGLE_GAP);
                            }
                            short_name = cut(&names[tab], name_room.max(1));
                            words.push((Word::Tab(tab), short_name.trim_end()));
                        }
                        let hidden = names.len() - fit.len();
                        if hidden > 0 && counted {
                            let next = (tab + 1..names.len())
                                .chain(0..tab)
                                .find(|n| !fit.contains(n))
                                .unwrap_or(tab);
                            more = format!("+{hidden}");
                            words.push((Word::More(next), more.as_str()));
                        }
                        tab_words = words;
                        toggles.insert(
                            0,
                            Toggle {
                                words: &tab_words,
                                active: Word::Tab(tab),
                            },
                        );
                    }
                    let shown = boxed.min(visible_width);
                    let max = natural.saturating_sub(shown);
                    let offset = app.pans.get(&key).copied().unwrap_or(0).min(max);

                    // one row for the border above the box's rows
                    let start = rows.len();
                    let first = lines.len() + 1 + start;
                    let hot =
                        app.hover_bar == Some(key) || app.drag.is_some_and(|drag| drag.key == key);
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
                    for (n, row) in block_rows.lines.iter().enumerate() {
                        let text = block_rows.texts[n]
                            .as_ref()
                            .map(|text| windowed(text, offset, shown));
                        if n > 0 || copy.is_none() {
                            rows.push(window(row, offset, shown), text);
                            continue;
                        }
                        let (label, style) = if copied == Some(key) {
                            ("copied", Style::new().fg(palette::get().accent))
                        } else if app.hover_copy == Some(key) {
                            (COPY_GLYPH, Style::new().fg(palette::get().accent))
                        } else {
                            (COPY_GLYPH, dim())
                        };
                        let (row, words) = header_row(row, offset, shown, &toggles, label, style);
                        rows.push(row, text);
                        toggle_spots.extend(words.into_iter().map(|(col, width, word)| {
                            ToggleSpot {
                                key: match word {
                                    Word::Tab(_) | Word::More(_) => box_key,
                                    _ => key,
                                },
                                line: first,
                                col,
                                width,
                                word,
                            }
                        }));
                    }
                    for run in runs {
                        let Some((col, width)) = panned(run.col, run.width, offset, shown) else {
                            continue;
                        };
                        let link = (index, numbered_in, run.index);
                        if hovered == Some(link) {
                            let row = &mut rows.lines[start + run.row];
                            paint(row, col, width, palette::get().accent);
                        }
                        spots.push(LinkSpot {
                            key: link,
                            line: first + run.row,
                            col,
                            width,
                            link: run.link.clone(),
                        });
                    }
                    if max > 0 {
                        rows.push(scroll_bar(shown, max, offset, hot), None);
                    }
                }
                let label = border_label(reply, today, visible_width);
                lines.append(framed(rows, palette::get().blue, None, label.as_deref()));
            }
            Entry::Thought(text) => wrapped("✻ ", "  ", dim(), text, usable, dim(), &mut lines),
            Entry::Tool(tool) => {
                let expanded = app.expanded.contains(&tool.id);
                let hovered = app.hover_fold.as_ref() == Some(&tool.id);
                let (rows, head) =
                    tool_view(tool, &app.cwd, usable, expanded, hovered, &mut app.rendered);
                if expandable(tool) {
                    folds.push(Fold {
                        id: tool.id.clone(),
                        first: lines.len(),
                        height: head,
                        rows: rows.len(),
                    });
                }
                lines.append(rows);
            }
            Entry::Permission { choice, .. } => {
                let pad = width.saturating_sub(choice.width());
                let line = Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(choice.clone(), Style::new().fg(palette::get().accent)),
                ]);
                lines.push_text(line, pad, cmr::Join::Break);
            }
            Entry::Notice(text) => {
                let text = cut(text, width);
                let pad = (width - text.width()) / 2;
                let line = Line::from(format!("{}{text}", " ".repeat(pad))).style(dim());
                lines.push_text(line, pad, cmr::Join::Break);
            }
        }
        lines.push_text(Line::default(), 0, cmr::Join::Break);
    }
    Built {
        lines,
        wides,
        copies,
        folds,
        links: spots,
        toggles: toggle_spots,
    }
}

/// The conversation, one column narrower with a scroll bar in the last
/// column when it is taller than `outer`.
fn draw_transcript(frame: &mut Frame, outer: Rect, app: &mut App) {
    let height = outer.height as usize;
    let mut area = outer;
    let mut width = (area.width as usize).max(4);
    let mut total = build_transcript(app, width, 0..0).lines.len();
    if total > height && area.width > 1 {
        // a narrower layout can be shorter, as a tool row drops its location
        let narrower = build_transcript(app, (area.width as usize - 1).max(4), 0..0)
            .lines
            .len();
        if narrower > height {
            area.width -= 1;
            width = (area.width as usize).max(4);
            total = narrower;
        }
    }
    app.page = height.saturating_sub(1).max(1);
    if app.scroll > 0 {
        app.scroll += total.saturating_sub(app.lines_seen);
    }
    app.lines_seen = total;
    app.scroll = app.scroll.min(total.saturating_sub(height));
    let end = total - app.scroll;
    let start = end.saturating_sub(height);
    let mut laid = start..end;
    if let Some(selection) = &mut app.selection {
        if let Some(pointer) = selection.pointer {
            selection.head = select::nearest(area, start..end, pointer);
        }
        let (from, to) = selection.ends();
        laid = start.min(from.line)..end.max(to.line + 1);
    }
    let Built {
        lines,
        wides,
        copies,
        folds,
        links: spots,
        toggles,
    } = build_transcript(app, width, laid);
    debug_assert_eq!(lines.len(), total);
    let on_screen = |first: usize, rows: usize| {
        let top = first.max(start);
        let bottom = (first + rows).min(end).max(top);
        Rect {
            x: area.x,
            y: area.y + (top - start) as u16,
            width: area.width,
            height: (bottom - top) as u16,
        }
    };
    app.folds = folds
        .iter()
        .map(|fold| FoldArea {
            area: on_screen(fold.first, fold.height),
            whole: on_screen(fold.first, fold.rows),
            id: fold.id.clone(),
        })
        .filter(|fold| fold.whole.height > 0)
        .collect();
    app.link_areas = spots
        .into_iter()
        .filter(|spot| (start..end).contains(&spot.line))
        .map(|spot| LinkArea {
            area: Rect::new(
                area.x + 2 + spot.col as u16,
                area.y + (spot.line - start) as u16,
                spot.width as u16,
                1,
            ),
            key: spot.key,
            target: spot.link.target,
            title: spot.link.title,
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
    app.toggles = toggles
        .iter()
        .filter(|toggle| (start..end).contains(&toggle.line))
        .map(|toggle| ToggleArea {
            area: Rect::new(
                area.x + 2 + toggle.col as u16,
                area.y + (toggle.line - start) as u16,
                toggle.width as u16,
                1,
            ),
            key: toggle.key,
            word: toggle.word,
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
    let Drawn { lines, texts } = lines;
    let visible: Vec<Line> = lines.into_iter().take(end).skip(start).collect();
    frame.render_widget(Paragraph::new(visible), area);
    let tint = Style::new().bg(palette::get().tint);
    if let Some(fold) = app
        .folds
        .iter()
        .find(|fold| app.hover_fold.as_ref() == Some(&fold.id))
    {
        frame.buffer_mut().set_style(fold.whole, tint);
    }
    if let Some(selection) = &app.selection {
        for line in start..end {
            let Some(Some(text)) = texts.get(line) else {
                continue;
            };
            let Some(cols) = selection.covers(line, text) else {
                continue;
            };
            let cells = Rect::new(
                area.x + cols.start as u16,
                area.y + (line - start) as u16,
                cols.len() as u16,
                1,
            );
            frame.buffer_mut().set_style(cells.intersection(area), tint);
        }
    }
    app.texts = select::Texts {
        area,
        start,
        lines: texts,
    };
    app.v_bar = None;
    if area.width < outer.width {
        let bar = VBar {
            area: Rect::new(area.right(), area.y, 1, outer.height),
            max: app.lines_seen.saturating_sub(height),
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

    /// The rows of the call's head.
    height: usize,

    /// The rows of the whole call, head included.
    rows: usize,
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

/// A toggle word on a block's header row on screen, found by the mouse.
pub struct ToggleArea {
    pub area: Rect,

    /// The entry and the block within it.
    pub key: (usize, usize),
    pub word: Word,
}

/// A link's text on screen, found by the mouse.
pub struct LinkArea {
    pub area: Rect,

    pub key: LinkKey,
    pub target: String,
    pub title: Option<String>,
}

/// The entry a link is in, the rendered markdown block that numbers it if
/// one does, and its number there.
pub type LinkKey = (usize, Option<usize>, usize);

/// An expandable tool call on screen, which a click anywhere on it folds or
/// unfolds.
pub struct FoldArea {
    /// The call's head rows.
    pub area: Rect,

    /// All of the call's rows.
    pub whole: Rect,
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
fn bubble(prompt: &Message, today: Time, width: usize) -> Drawn {
    let widest = (width * 7 / 10).max(5);
    let mut rows = Drawn::default();
    for paragraph in prompt.text.split('\n') {
        let pieces = wrap_joined(paragraph, widest - 4);
        if pieces.is_empty() {
            rows.push_text(Line::default(), 0, cmr::Join::Break);
        }
        for (piece, join) in pieces {
            rows.push_text(Line::from(piece), 0, join);
        }
    }
    let label = border_label(prompt, today, widest - 4);
    framed(rows, palette::get().accent, Some(width), label.as_deref())
}

/// The time `message` began, and the turn's length when it has one, or
/// `None` when the message has no time or the label and its spaces are
/// wider than `room`.
fn border_label(message: &Message, today: Time, room: usize) -> Option<String> {
    let mut label = message.time?.label(today);
    if let Some(took) = message.took {
        label.push_str(" · ");
        label.push_str(&elapsed_text(took));
    }
    (label.width() + 2 <= room).then_some(label)
}

/// `paragraph` wrapped at `width`, each row with how it follows the one
/// above.
fn wrap_joined(paragraph: &str, width: usize) -> Vec<(String, cmr::Join)> {
    let mut at = 0;
    textwrap::wrap(paragraph, width)
        .into_iter()
        .enumerate()
        .map(|(n, piece)| {
            // a row after the first starts past the whitespace it broke
            // at, or right where the row above ended when it split a word
            let found = paragraph[at..].find(piece.as_ref()).map_or(at, |i| at + i);
            let join = match n {
                0 => cmr::Join::Break,
                _ if found > at => cmr::Join::Space,
                _ => cmr::Join::Word,
            };
            at = found + piece.len();
            (piece.into_owned(), join)
        })
        .collect()
}

/// `rows` inside a rounded border of colour `border` with a space of padding
/// each side. With `Some(width)` the box sits against the right edge of that
/// width, and otherwise against the left. A `label` sits dim in the top
/// border at the corner nearest that edge, and the box widens to hold it.
fn framed(rows: Drawn, border: Color, right_edge: Option<usize>, label: Option<&str>) -> Drawn {
    let label_width = label.map_or(0, |label| label.width() + 2);
    let inner = rows
        .lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(label_width);
    let pad = right_edge.map_or(0, |width| width.saturating_sub(inner + 4));
    let edge = Style::new().fg(border);
    let side = |left: &str, middle: &str, right: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(pad)),
            Span::styled(format!("{left}{}{right}", middle.repeat(inner + 2)), edge),
        ])
    };
    let mut lines = Drawn::default();
    let top = match label {
        None => side("╭", "─", "╮"),
        Some(label) => {
            let run = "─".repeat(inner + 1 - label_width);
            let (before, after) = match right_edge {
                Some(_) => (format!("╭{run} "), " ─╮".to_string()),
                None => ("╭─ ".to_string(), format!(" {run}╮")),
            };
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(before, edge),
                Span::styled(label.to_string(), dim()),
                Span::styled(after, edge),
            ])
        }
    };
    lines.push(top, None);
    for (row, mut text) in rows.lines.into_iter().zip(rows.texts) {
        let gap = inner - row.width();
        let mut spans = vec![Span::raw(" ".repeat(pad)), Span::styled("│ ", edge)];
        spans.extend(row.spans);
        spans.push(Span::styled(format!("{} │", " ".repeat(gap)), edge));
        if let Some(text) = &mut text {
            text.col += pad + 2;
        }
        lines.push(Line::from(spans), text);
    }
    lines.push(side("╰", "─", "╯"), None);
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
    out: &mut Drawn,
) {
    let inner = width.saturating_sub(first.width()).max(1);
    let mut lead = first;
    for paragraph in text.split('\n') {
        let pieces = wrap_joined(paragraph, inner);
        if pieces.is_empty() {
            let line = Line::from(Span::styled(lead.to_string(), lead_style));
            out.push_text(line, lead.width(), cmr::Join::Break);
            lead = rest;
        }
        for (piece, join) in pieces {
            let line = Line::from(vec![
                Span::styled(lead.to_string(), lead_style),
                Span::styled(piece, style),
            ]);
            out.push_text(line, lead.width(), join);
            lead = rest;
        }
    }
}

fn tool_block(tool: &Tool, cwd: &Path, width: usize) -> Drawn {
    let word = if tool.agent {
        "agent"
    } else {
        kind_word(tool.kind)
    };
    tool_rows(tool, cwd, width, word, "")
}

/// A tool call's head: the dot row, and a row under it unless the call is
/// done with nothing to add. `word` goes in the kind column and `suffix` dim
/// after the title.
fn tool_rows(tool: &Tool, cwd: &Path, width: usize, word: &str, suffix: &str) -> Drawn {
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
    let (dot, status, status_style) = if tool.cancelled {
        (dim(), "cancelled", dim())
    } else {
        (dot, status, status_style)
    };
    let dot = Span::styled("● ", dot);
    let mark = dot.width();
    let head = [
        dot,
        Span::styled(format!("{word:<6}"), bold()),
        Span::raw(" "),
    ];
    let room = width.saturating_sub(head.iter().map(Span::width).sum::<usize>() + suffix.width());
    let mut first = head.to_vec();
    first.push(Span::raw(cut(&tool.title, room)));
    first.push(Span::styled(suffix.to_string(), dim()));

    let lead = Span::styled("  ⎿  ", dim());
    let lead_width = lead.width();
    let room = width.saturating_sub(lead_width);
    let mut second = vec![lead];
    let mut detail = Vec::new();
    let (added, removed) = tool.changed;
    let mut used = 0;
    if added + removed > 0 {
        let plus = format!("+{added}");
        let minus = format!(" −{removed}");
        used = plus.width() + minus.width();
        detail.push(Span::styled(plus, Style::new().fg(palette::get().green)));
        detail.push(Span::styled(minus, Style::new().fg(palette::get().red)));
    }

    // the location gives way when the row cannot hold it
    let place = location(tool, cwd).filter(|place| place.width() + 2 + used <= room);
    if let Some(place) = place {
        second.push(Span::styled(place, dim()));
        if used > 0 {
            second.push(Span::raw("  "));
        }
    }
    second.extend(detail);
    let mut rows = Drawn::default();
    rows.push_text(Line::from(first), mark, cmr::Join::Break);
    if second.len() == 1 {
        if tool.status == ToolCallStatus::Completed {
            return rows;
        }
        second.push(Span::styled(status, status_style));
    }
    rows.push_text(Line::from(second), lead_width, cmr::Join::Break);
    rows
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
    rendered: &mut Rendered,
) -> (Drawn, usize) {
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
    lines.lines[0] = with_marker(lines.lines[0].clone(), width, expanded, hovered);
    if expanded && tool.children.is_empty() {
        let inner = width.saturating_sub(SUBAGENT_INDENT.len());
        lines.extend(rendered.expanded(tool, inner));
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

/// The rows of an edit's diffs: removed lines red after a `-`, added lines
/// green after a `+`, and the unchanged lines around them dim. A run of
/// unchanged lines past the context shows as a count.
fn diff_rows(content: &[ToolCallContent], width: usize) -> Drawn {
    let diffs: Vec<_> = content
        .iter()
        .filter_map(|content| match content {
            ToolCallContent::Diff(diff) => Some(diff),
            _ => None,
        })
        .collect();
    let palette = palette::get();
    let mut rows = Drawn::default();
    let mut push = |line| rows.push_text(line, 0, cmr::Join::Break);
    for diff in &diffs {
        if diffs.len() > 1 {
            let path = diff.path.display().to_string();
            push(Line::styled(cut(&path, width), dim()));
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
                push(Line::styled(format!("⋯ {hidden} unchanged"), dim()));
                hidden = 0;
            }
            let text = cut(&format!("{mark} {text}"), width);
            push(match mark {
                '-' => Line::styled(text, Style::new().fg(palette.red)),
                '+' => Line::styled(text, Style::new().fg(palette.green)),
                _ => Line::styled(text, dim()),
            });
        }
        if hidden > 0 {
            push(Line::styled(format!("⋯ {hidden} unchanged"), dim()));
        }
    }
    rows
}

/// The rows of a command's output, laid out as markdown, since the adapter
/// sends it as a fenced block.
fn output_rows(content: &[ToolCallContent], width: usize) -> Drawn {
    let mut rows = Drawn::default();
    for text in output(content) {
        for (row, mark) in cmr::layout_marked(&cmr::parse(text), width) {
            let line = row
                .into_iter()
                .map(|(text, style)| Span::styled(text, screen_style(style)))
                .collect();
            rows.push_text(trim_end(line), mark.lead, mark.join);
        }
    }
    rows
}

/// A subagent's call: folded to its two rows, or expanded with what the
/// subagent did indented under it.
fn subagent_block(tool: &Tool, cwd: &Path, width: usize, expanded: bool) -> Drawn {
    let mut lines = subagent_head(tool, cwd, width);
    if !expanded {
        return lines;
    }
    let inner = width.saturating_sub(SUBAGENT_INDENT.len());
    for child in &tool.children {
        let mut rows = match child {
            Child::Tool(call) => tool_block(call, cwd, inner),
            Child::Text(text) => {
                let mut rows = Drawn::default();
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
                let mut rows = Drawn::default();
                wrapped("✻ ", "  ", dim(), text, inner, dim(), &mut rows);
                rows
            }
            Child::User(text) => {
                let mut rows = Drawn::default();
                wrapped("> ", "  ", dim(), text, inner, dim(), &mut rows);
                rows
            }
        };
        for row in &mut rows.lines {
            row.spans.insert(0, Span::raw(SUBAGENT_INDENT));
        }
        rows.indent(SUBAGENT_INDENT.len());
        lines.append(rows);
    }
    lines
}

fn subagent_head(tool: &Tool, cwd: &Path, width: usize) -> Drawn {
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

pub fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width > room - 1 {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    used += 1;
    // a wide char that did not fit leaves the row one column short
    out.push_str(&" ".repeat(room - used));
    out
}

/// `text` followed by spaces to `width` columns.
pub fn pad_right(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// `text` preceded by spaces to `width` columns.
pub fn pad_left(text: &str, width: usize) -> String {
    format!("{}{text}", " ".repeat(width.saturating_sub(text.width())))
}

fn draw_permission(frame: &mut Frame, over: Rect, app: &App) {
    let Some(permission) = &app.permission else {
        return;
    };
    let mut lines = Vec::new();
    let (added, removed) = permission.changed;
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
    let wanted = content_width.max(permission.title.width() + 2) + 4;
    let width = (wanted as u16).clamp(24.min(over.width), over.width);
    let height = (lines.len() as u16 + 2).min(over.height);
    let area = Rect {
        x: over.x + (over.width - width) / 2,
        y: over.y + (over.height - height) / 2,
        width,
        height,
    };
    let title = cut(&permission.title, (width as usize).saturating_sub(4));
    let mut block =
        pane(&title, true).title_bottom(Line::styled("esc cancel", dim()).right_aligned());
    if !app.queued.is_empty() {
        block = block.title_bottom(Line::styled(format!("{} more", app.queued.len()), dim()));
    }
    clear(frame, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The open form in the prompt's place, scrolled to keep the `focus` lines in
/// view, under a rule carrying its message and keys.
fn draw_form(
    frame: &mut Frame,
    rule: Rect,
    area: Rect,
    app: &App,
    lines: Vec<Line<'static>>,
    focus: Range<usize>,
) {
    let Some(asked) = &app.form else {
        return;
    };
    let keys = match app.forms.len() {
        0 => "enter pick   esc dismiss".to_string(),
        more => format!("{more} more   enter pick   esc dismiss"),
    };
    let width = rule.width as usize;
    let title = cut(&asked.form.message, width.saturating_sub(keys.width() + 9));
    let rest = width.saturating_sub(title.width() + keys.width() + 8);
    let title_style = Style::new()
        .fg(palette::get().blue)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("── ", dim()),
            Span::styled(title, title_style),
            Span::styled(format!(" {} ", "─".repeat(rest)), dim()),
            Span::styled(keys, dim()),
            Span::styled(" ──", dim()),
        ])),
        rule,
    );
    let skip = focus
        .end
        .saturating_sub(area.height as usize)
        .min(focus.start);
    let lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    let lead = PROMPT_LEAD.len() as u16;
    let body = Rect {
        x: area.x + lead.min(area.width),
        width: area.width.saturating_sub(lead),
        ..area
    };
    frame.render_widget(Paragraph::new(lines), body);
}

/// The form's lines at `width`, and the lines to keep in view: the cursor's
/// row through the end of the preview it shows.
fn form_lines(form: &Form, width: usize) -> (Vec<Line<'static>>, Range<usize>) {
    let rows = form.rows();
    let preview = form.focused_preview();
    let mut lines: Vec<Line> = Vec::new();
    let mut focus = 0..0;
    let mut shown_field = None;
    for (index, row) in rows.iter().enumerate() {
        let field = form::row_field(*row);
        if field != shown_field {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            if let Some(field) = field.map(|field| &form.fields[field]) {
                lines.push(Line::styled(cut(&field.title, width), bold()));
                for piece in field
                    .description
                    .iter()
                    .flat_map(|text| textwrap::wrap(text, width))
                {
                    lines.push(Line::styled(piece.into_owned(), dim()));
                }
            }
            shown_field = field;
        }
        if index == form.cursor {
            focus = lines.len()..lines.len() + 1;
        }
        let line = form_row(form, *row, width);
        lines.push(if index == form.cursor {
            line.style(Style::new().bg(palette::get().selection))
        } else {
            line
        });
        let last_of_field = rows.get(index + 1).and_then(|next| form::row_field(*next)) != field;
        if let Some((previewed, text)) = preview
            && field == Some(previewed)
            && last_of_field
        {
            lines.extend(preview_lines(text, width));
            focus.end = lines.len();
        }
    }
    if let Some(error) = &form.error {
        lines.push(Line::styled(
            cut(error, width),
            Style::new().fg(palette::get().yellow),
        ));
    }
    (lines, focus)
}

/// An option's preview in a dim box at most `width` wide, each line cut to
/// fit.
fn preview_lines(preview: &str, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(4);
    let text: Vec<String> = preview
        .lines()
        .map(|line| cut(&line.replace('\t', "    "), room))
        .collect();
    let inner = text.iter().map(|line| line.width()).max().unwrap_or(0);
    let mut lines = vec![Line::styled(format!("╭{}╮", "─".repeat(inner + 2)), dim())];
    for line in &text {
        lines.push(Line::styled(
            format!("│ {} │", pad_right(line, inner)),
            dim(),
        ));
    }
    lines.push(Line::styled(format!("╰{}╯", "─".repeat(inner + 2)), dim()));
    lines
}

fn form_row(form: &Form, row: Row, width: usize) -> Line<'static> {
    let accent = Style::new().fg(palette::get().accent);
    match row {
        Row::Option(field, option) => {
            let (choice, mark) = match &form.fields[field].kind {
                FormKind::Choice { options, picked } => {
                    let mark = if *picked == Some(option) {
                        "● "
                    } else {
                        "○ "
                    };
                    (&options[option], mark)
                }
                FormKind::Many { options, picked } => {
                    let mark = if picked[option] { "[x] " } else { "[ ] " };
                    (&options[option], mark)
                }
                _ => return Line::default(),
            };
            let title = cut(&choice.title, width.saturating_sub(mark.width()));
            let mut spans = vec![Span::styled(mark, accent), Span::raw(title.clone())];
            if let Some(description) = &choice.description {
                let room = width.saturating_sub(mark.width() + title.width() + 2);
                if room > 1 {
                    spans.push(Span::styled(format!("  {}", cut(description, room)), dim()));
                }
            }
            Line::from(spans)
        }
        Row::Text(field) => {
            let FormKind::Text { text, .. } = &form.fields[field].kind else {
                return Line::default();
            };
            let shown = if text.is_empty() {
                Span::styled("type here", dim())
            } else {
                Span::raw(cut(text, width.saturating_sub(2)))
            };
            Line::from(vec![Span::styled("› ", accent), shown])
        }
        Row::Toggle(field) => {
            let FormKind::Toggle(on) = form.fields[field].kind else {
                return Line::default();
            };
            let mark = if on { "[x] yes" } else { "[ ] no" };
            Line::styled(mark, accent)
        }
        Row::Submit => Line::styled("submit", accent.add_modifier(Modifier::BOLD)),
    }
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
        .map(|entry| entry.fill.trim_end().width())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(shown)
        .map(|(index, entry)| {
            let name = pad_right(entry.fill.trim_end(), name_width);
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
    clear(frame, area);
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
    clear(frame, area);
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
    let all = prompt::wrap_rows(text, width);
    let shown = max_rows.max(PROMPT_ROWS);
    let cursor = cursor.min(text.len());
    let cursor_row = prompt::cursor_row(&all, cursor);
    let skipped = all.len().saturating_sub(shown).min(cursor_row);
    let end = (skipped + shown).min(all.len());
    let rows = all[skipped..end]
        .iter()
        .map(|row| text[row.clone()].to_string())
        .collect();
    let column = text[all[cursor_row].start..cursor].width();
    let cursor = (
        (PROMPT_LEAD.len() + column.min(width.max(1) - 1)) as u16,
        (cursor_row - skipped) as u16,
    );
    InputView {
        rows,
        starts_here: skipped == 0,
        cursor,
    }
}

/// The first line of `text` that has something on it.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// The widest and tallest the history box gets, in columns and rows.
const HISTORY_WIDTH: u16 = 60;
const HISTORY_ROWS: usize = 10;

/// The picker box hanging under a header chip, its last row above `floor`.
pub fn draw_menu(frame: &mut Frame, app: &mut App, floor: u16, now: Time) {
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
        MenuTarget::History => {
            let rows = app
                .history
                .iter()
                .rev()
                .map(|(text, time)| {
                    let lines = text.lines().count();
                    let note = (lines > 1).then(|| format!("{lines} lines"));
                    let age = time.map(|time| time.age(now));
                    let note = match (note, age) {
                        (Some(note), Some(age)) => Some(format!("{note}  {age}")),
                        (note, age) => note.or(age),
                    };
                    (first_line(text).to_string(), note, false)
                })
                .collect();
            ("history".to_string(), rows)
        }
    };
    let screen = frame.area();
    let row_width = |(name, note, _): &(String, Option<String>, bool)| {
        2 + name.width() + note.as_ref().map_or(0, |note| 2 + note.width())
    };
    let wanted = rows
        .iter()
        .map(row_width)
        .max()
        .unwrap_or(0)
        .max(title.width() + 2)
        + 4;
    let width = (wanted as u16).min(screen.width.saturating_sub(4).max(24).min(screen.width));
    let history = matches!(menu.target, MenuTarget::History);
    let width = if history {
        width.min(HISTORY_WIDTH)
    } else {
        width
    };
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
        MenuTarget::Overflow(_) | MenuTarget::History => None,
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
        .min((floor as usize).saturating_sub(3 + error))
        .min(if history { HISTORY_ROWS } else { usize::MAX })
        .max(1);
    let first = (menu.cursor + 1).saturating_sub(visible);
    let height = ((visible + error) as u16 + 2).min(floor);
    let area = Rect {
        x: menu.anchor.min(screen.width - width),
        y: floor.saturating_sub(height),
        width,
        height,
    };
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(index, (name, note, current))| {
            let line = if history {
                history_row(name, note.as_deref(), inner)
            } else {
                let mark = if *current { "● " } else { "  " };
                let name = cut(name, inner.saturating_sub(2));
                let mut spans = vec![Span::raw(mark), Span::raw(name.clone())];
                if let Some(note) = note {
                    let room = inner.saturating_sub(2 + name.width() + 2);
                    spans.push(Span::styled(format!("  {}", cut(note, room)), dim()));
                }
                Line::from(spans)
            };
            if index == menu.cursor {
                line.style(Style::new().bg(palette::get().selection))
            } else if history && index % 2 == 1 {
                line.style(Style::new().bg(palette::get().tint))
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
    clear(frame, area);
    frame.render_widget(Paragraph::new(lines).block(pane(&title, true)), area);
    app.menu_area = Some(area);
    app.menu_first = first;
}

/// A history row: a dim `› `, the prompt's first line, and `note` dim
/// against the right edge, padded to `inner` columns.
fn history_row(name: &str, note: Option<&str>, inner: usize) -> Line<'static> {
    let note = note.unwrap_or("");
    let room = inner.saturating_sub(2 + note.width() + if note.is_empty() { 0 } else { 2 });
    let name = cut(name, room);
    let fill = inner.saturating_sub(2 + name.width() + note.width());
    Line::from(vec![
        Span::styled("› ", dim()),
        Span::raw(name),
        Span::raw(" ".repeat(fill)),
        Span::styled(note.to_string(), dim()),
    ])
}

pub fn dim() -> Style {
    match palette::get().muted {
        Some(muted) => Style::new().fg(muted),
        None => Style::new().add_modifier(Modifier::DIM),
    }
}

/// Clear `area` for a box drawn over the screen, back to the base style.
fn clear(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_style(area, palette::base());
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

    #[test]
    fn the_working_time_reads_in_seconds_then_minutes_then_hours() {
        let text = |seconds| elapsed_text(Duration::from_secs(seconds));
        assert_eq!(text(0), "0s");
        assert_eq!(text(59), "59s");
        assert_eq!(text(65), "1m 05s");
        assert_eq!(text(3720), "1h 02m");
    }

    fn flat(parts: &[Part]) -> Vec<Line<'static>> {
        parts
            .iter()
            .flat_map(|part| match part {
                Part::Flow { rows, .. } | Part::Block(KeptBlock { rows, .. }) => rows.lines.clone(),
                Part::Tabs { tabs, .. } => tabs[0].rows.lines.clone(),
            })
            .collect()
    }

    fn drawn(lines: Vec<Line<'static>>) -> Drawn {
        Drawn {
            texts: vec![None; lines.len()],
            lines,
        }
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
            cancelled: false,
            changed: (0, 0),
            revision: 0,
            agent: false,
            children: Vec::new(),
        }
    }

    fn rows(tool: &Tool, width: usize) -> Vec<String> {
        tool_block(tool, Path::new("/work"), width)
            .lines
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
        edit.changed = crate::diff::size(&edit.content);
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
    fn a_cursor_on_a_break_space_past_the_width_draws_at_the_end_of_its_row() {
        let view = input_view("one two three", 7, 7, 10);
        assert_eq!(view.rows, ["one two ", "three"]);
        assert_eq!(view.cursor, (2 + 6, 0));
        let view = input_view("one two three", 8, 7, 10);
        assert_eq!(view.cursor, (2, 1));
    }

    #[test]
    fn the_window_follows_a_cursor_above_the_last_rows() {
        let text = "a ".repeat(30);
        let view = input_view(&text, 0, 4, 5);
        assert!(view.starts_here);
        assert_eq!(view.cursor, (2, 0));
    }

    #[test]
    fn cut_counts_columns_and_never_splits_a_wide_char() {
        assert_eq!(cut("日本語", 6), "日本語");
        assert_eq!(cut("日本語", 5), "日本…");
        assert_eq!(cut("日本語", 4), "日… ");
        assert_eq!(cut("日本語", 4).width(), 4);
        assert_eq!(cut("a😀bc", 3), "a… ");
        assert_eq!(cut("abc", 2), "a…");
        assert_eq!(cut("日本語", 0), "");
        assert_eq!(cut("日本語", 1), "…");
    }

    #[test]
    fn the_cursor_column_is_the_display_width_before_it() {
        let text = "日本語";
        let view = input_view(text, 6, 20, 10);
        assert_eq!(view.cursor, (2 + 4, 0));
        let view = input_view("a😀b", 5, 20, 10);
        assert_eq!(view.cursor, (2 + 3, 0));
        let view = input_view("日本語日", 12, 6, 10);
        assert_eq!(view.cursor, (2 + 2, 1));
    }

    #[test]
    fn the_cursor_is_on_the_row_after_a_newline() {
        let view = input_view("ab\ncd", 3, 20, 10);
        assert_eq!(view.cursor, (2, 1));
        let view = input_view("ab\ncd", 2, 20, 10);
        assert_eq!(view.cursor, (4, 0));
        let view = input_view("ab\n", 3, 20, 10);
        assert_eq!(view.cursor, (2, 1));
        assert_eq!(view.height(), 2);
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
    fn a_queued_row_draws_its_label_in_the_accent_and_its_text_in_the_foreground() {
        let palette = palette::get();
        let waiting: VecDeque<String> = ["one", "two", "three", "four"].map(str::to_string).into();
        let lines = waiting_lines(&waiting, 40);
        let colours = |line: &Line| -> Vec<(String, Option<Color>)> {
            line.spans
                .iter()
                .filter(|span| !span.content.trim().is_empty())
                .map(|span| (span.content.trim().to_string(), span.style.fg))
                .collect()
        };
        assert_eq!(
            colours(&lines[0]),
            [
                ("queued".to_string(), Some(palette.accent)),
                ("one".to_string(), Some(palette.foreground)),
            ]
        );
        assert_eq!(
            colours(&lines[2]),
            [
                ("+ 2 more".to_string(), Some(palette.foreground)),
                ("queued".to_string(), Some(palette.accent)),
            ]
        );
    }

    const TODAY: Time = Time {
        year: 2026,
        month: 11,
        day: 10,
        hour: 15,
        minute: 0,
    };

    const EARLIER: Time = Time {
        hour: 14,
        minute: 32,
        ..TODAY
    };

    fn untimed(text: &str) -> Message {
        Message::new(text.to_string(), None)
    }

    #[test]
    fn a_prompts_border_shows_its_time_on_the_right() {
        let prompt = Message::new("hello there".to_string(), Some(EARLIER));
        let lines = plain(&bubble(&prompt, TODAY, 40).lines);
        assert_eq!(lines[0], format!("{}╭─── 2:32 PM ─╮", " ".repeat(25)));
        assert_eq!(lines[1], format!("{}│ hello there │", " ".repeat(25)));
    }

    #[test]
    fn a_short_prompt_widens_its_bubble_to_hold_the_time() {
        let prompt = Message::new("hi".to_string(), Some(EARLIER));
        let lines = plain(&bubble(&prompt, TODAY, 40).lines);
        assert_eq!(lines[0], format!("{}╭─ 2:32 PM ─╮", " ".repeat(27)));
        assert_eq!(lines[1], format!("{}│ hi        │", " ".repeat(27)));
    }

    #[test]
    fn a_replys_border_shows_its_time_on_the_left_with_the_turn_length() {
        let mut reply = Message::new("the answer is here".to_string(), Some(EARLIER));
        reply.took = Some(Duration::from_secs(41));
        let label = border_label(&reply, TODAY, 60);
        let rows = drawn(vec![Line::from("the answer is here")]);
        let lines = plain(&framed(rows, Color::Blue, None, label.as_deref()).lines);
        assert_eq!(lines[0], "╭─ 2:32 PM · 41s ────╮");
        assert_eq!(lines[1], "│ the answer is here │");
    }

    #[test]
    fn a_message_from_an_earlier_day_also_shows_the_date() {
        let yesterday = Time { day: 9, ..EARLIER };
        let prompt = Message::new("hello there".to_string(), Some(yesterday));
        let lines = plain(&bubble(&prompt, TODAY, 60).lines);
        assert!(lines[0].ends_with("╭─ 2026-11-09 2:32 PM ─╮"));
    }

    #[test]
    fn a_box_too_narrow_for_the_time_leaves_it_out() {
        let prompt = Message::new("hi".to_string(), Some(EARLIER));
        let lines = plain(&bubble(&prompt, TODAY, 10).lines);
        assert_eq!(lines[0], format!("{}╭────╮", " ".repeat(4)));
    }

    #[test]
    fn a_replayed_message_without_a_time_shows_none() {
        let reply = untimed("from last time");
        assert_eq!(border_label(&reply, TODAY, 60), None);
        let lines = plain(&bubble(&reply, TODAY, 40).lines);
        assert!(lines[0].trim_start().starts_with("╭──────────────"));
        assert!(!lines[0].contains(':'));
    }

    #[test]
    fn a_short_message_gets_a_bubble_as_wide_as_its_text_plus_four() {
        let lines = plain(&bubble(&untimed("hi"), TODAY, 40).lines);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], format!("{}╭────╮", " ".repeat(34)));
        assert_eq!(lines[1], format!("{}│ hi │", " ".repeat(34)));
        assert_eq!(lines[2], format!("{}╰────╯", " ".repeat(34)));
    }

    #[test]
    fn a_bubble_sits_flush_against_the_right_edge_at_any_width() {
        for width in [20, 60, 100] {
            for line in bubble(&untimed("hello there"), TODAY, width).lines {
                assert_eq!(line.width(), width);
            }
        }
    }

    #[test]
    fn a_long_message_wraps_inside_a_bubble_of_seventy_percent_of_the_width() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let lines = bubble(&untimed(text), TODAY, 40).lines;
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
        let lines = plain(&framed(drawn(rows), Color::Blue, None, None).lines);
        assert_eq!(lines[0], "╭───────────────╮");
        assert_eq!(lines[1], "│ short         │");
        assert_eq!(lines[2], "│ a longer line │");
        assert_eq!(lines[3], "╰───────────────╯");
    }

    #[test]
    fn a_bubble_border_takes_the_given_colour_and_has_no_background() {
        let lines = framed(drawn(vec![Line::from("hi")]), Color::Red, Some(20), None).lines;
        for span in lines.iter().flat_map(|line| &line.spans) {
            assert_eq!(span.style.bg, None);
        }
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_wide_code_block_in_a_language_pans_by_default_and_one_in_text_or_none_wraps() {
        let long = "x".repeat(30);
        for (lang, wraps) in [("rust", false), ("sh", false), ("text", true), ("", true)] {
            let parts = markdown(&format!("```{lang}\n{long}\n```"), 20);
            let [
                Part::Block(KeptBlock {
                    wrapped: Some(wrapped),
                    ..
                }),
            ] = &parts[..]
            else {
                panic!("expected one block that can wrap");
            };
            assert_eq!(wrapped.default, wraps, "{lang}");
        }
    }

    #[test]
    fn a_wide_table_pans_with_no_wrapped_rows() {
        let text = "| name | note |\n|---|---|\n| a | a rather long note indeed |";
        let parts = markdown(text, 12);
        assert!(matches!(
            &parts[..],
            [Part::Block(KeptBlock { wrapped: None, .. })]
        ));
    }

    #[test]
    fn a_code_block_inside_a_rendered_markdown_block_wraps() {
        let long = "y".repeat(40);
        let text = format!("````markdown\n# T\n\n```rust\nlet s = \"{long}\";\n```\n````");
        let parts = markdown(&text, 30);
        let [
            Part::Block(KeptBlock {
                rendered: Some(view),
                ..
            }),
        ] = &parts[..]
        else {
            panic!("expected one rendered block");
        };
        let rows = plain(&view.rows.lines);
        assert!(rows.iter().any(|row| row.starts_with("│ ┆ ")), "{rows:?}");
        assert!(rows.iter().all(|row| !row.contains('…')), "{rows:?}");
        assert!(rows.iter().all(|row| row.width() <= 30), "{rows:?}");
    }

    #[test]
    fn a_code_block_is_as_wide_as_its_longest_line_plus_the_gutter() {
        let parts = markdown("```rs\nabcdefghij\nabc\n```", 20);
        let [Part::Block(KeptBlock { natural, rows, .. })] = &parts[..] else {
            panic!("expected one block");
        };
        assert_eq!(*natural, 14);
        assert_eq!(plain(&rows.lines), ["┌ rs", "│ abcdefghij", "│ abc"]);
    }

    #[test]
    fn a_table_is_as_wide_as_its_longest_row_with_the_padding_trimmed() {
        let text = "| name | note |\n|---|---|\n| a | a rather long note indeed |";
        let parts = markdown(text, 12);
        let [Part::Block(KeptBlock { natural, rows, .. })] = &parts[..] else {
            panic!("expected one block");
        };
        let widest = rows.lines.iter().map(Line::width).max().unwrap();
        assert_eq!(*natural, widest);
        assert!(widest > 12 && widest < 40, "{widest}");
    }

    #[test]
    fn a_table_that_fits_the_width_is_laid_out_with_the_prose() {
        let parts = markdown("| a | b |\n|---|---|\n| 1 | 2 |\n\nafter", 20);
        assert!(matches!(&parts[..], [Part::Flow { .. }]));
    }

    fn rendered_of(text: &str, width: usize) -> Vec<String> {
        let parts = markdown(text, width);
        let [
            Part::Block(KeptBlock {
                rendered: Some(view),
                ..
            }),
        ] = &parts[..]
        else {
            panic!("expected one markdown block");
        };
        plain(&view.rows.lines)
    }

    #[test]
    fn a_markdown_block_is_drawn_as_markdown_inside_its_box() {
        let rows = rendered_of("```markdown\n# Title\n\n- one\n```", 40);
        assert_eq!(rows, ["┌ markdown", "│ Title", "│", "│ • one"]);
    }

    #[test]
    fn a_three_backtick_fence_in_a_four_backtick_markdown_block_draws_as_a_code_block_inside() {
        let rows = rendered_of(
            "````md\nbefore\n\n```rust\nfn main() {}\n```\n\nafter\n````",
            40,
        );
        assert_eq!(
            rows,
            [
                "┌ md",
                "│ before",
                "│",
                "│ ┌ rust",
                "│ │ fn main() {}",
                "│",
                "│ after",
            ]
        );
    }

    #[test]
    fn a_code_block_in_another_language_has_no_markdown_view() {
        let parts = markdown("```rust\nfn main() {}\n```", 40);
        assert!(matches!(
            &parts[..],
            [Part::Block(KeptBlock { rendered: None, .. })]
        ));
    }

    /// The agent's text in `scenarios/tabs.json`.
    fn tabs_text() -> String {
        let scenario = crate::scenario::Scenario::parse(include_str!("../scenarios/tabs.json"));
        let step = &scenario.unwrap().acts[0].steps[0];
        let crate::scenario::Step::Update(
            agent_client_protocol::v1::schema::SessionUpdate::AgentMessageChunk(chunk),
        ) = step
        else {
            panic!("the first step is the agent's text");
        };
        let ContentBlock::Text(text) = &chunk.content else {
            panic!("the chunk is text");
        };
        text.text.clone()
    }

    #[test]
    fn adjacent_fences_that_name_tabs_draw_as_one_box() {
        let parts = markdown(&tabs_text(), 60);
        let boxes: Vec<(usize, &[String])> = parts
            .iter()
            .filter_map(|part| match part {
                Part::Tabs { first, names, .. } => Some((*first, names.as_slice())),
                _ => None,
            })
            .collect();
        let owned = ["owned".to_string(), "borrowed".to_string()];
        let before = ["before".to_string(), "after".to_string()];
        assert_eq!(boxes, [(1, &owned[..]), (4, &before[..])]);
        assert!(!parts.iter().any(|part| matches!(part, Part::Block(_))));
        let Part::Tabs { tabs, .. } = &parts[3] else {
            panic!("the markdown tabs follow the prose");
        };
        assert!(tabs.iter().all(|tab| tab.rendered.is_some()));
    }

    #[test]
    fn a_fence_without_a_tab_name_between_two_with_one_breaks_the_box() {
        let text = "```rust tab=\"a\"\na\n```\n```rust\nmid\n```\n```rust tab=\"b\"\nb\n```";
        let parts = markdown(text, 40);
        let shape: Vec<&str> = parts
            .iter()
            .map(|part| match part {
                Part::Flow { .. } => "flow",
                Part::Block(_) => "block",
                Part::Tabs { .. } => "tabs",
            })
            .collect();
        assert_eq!(shape, ["tabs", "flow", "block", "flow", "tabs"]);
    }

    #[test]
    fn the_names_that_fit_come_first_and_the_shown_tab_stays_among_them() {
        let names: Vec<String> = ["one", "two", "three"].map(String::from).into();
        assert_eq!(fitting_tabs(&names, 0, 40), [0, 1, 2]);

        // three columns of gap, then `one two +1`
        assert_eq!(fitting_tabs(&names, 0, 13), [0, 1]);
        assert_eq!(fitting_tabs(&names, 2, 13), [2]);
        assert_eq!(fitting_tabs(&names, 2, 15), [0, 2]);
        assert!(fitting_tabs(&names, 0, 2).is_empty());
    }

    #[test]
    fn every_code_block_is_a_block_of_its_own_with_its_text_to_copy() {
        let parts = markdown(
            "before\n\n```\nfn main() {\n    run();\n}\n```\n\nafter",
            40,
        );
        let [
            Part::Flow { .. },
            Part::Block(KeptBlock { copy, .. }),
            Part::Flow { .. },
        ] = &parts[..]
        else {
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

    fn runs_of(parts: &[Part]) -> Vec<(usize, usize, usize, usize, String)> {
        parts
            .iter()
            .flat_map(|part| match part {
                Part::Flow { runs, .. } => runs
                    .iter()
                    .map(|run| {
                        (
                            run.row,
                            run.col,
                            run.width,
                            run.index,
                            run.link.target.clone(),
                        )
                    })
                    .collect(),
                Part::Block(_) | Part::Tabs { .. } => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn a_link_keeps_its_index_in_the_entry_when_blocks_are_laid_out_one_at_a_time() {
        let text =
            "see [a](foam:x) and [b](file:y.rs#2)\n\n```\ncode\n```\n\nthen [c](https://z.test)";
        let parts = markdown(text, 60);
        assert_eq!(
            runs_of(&parts),
            [
                (0, 4, 1, 0, "foam:x".to_string()),
                (0, 10, 1, 1, "file:y.rs#2".to_string()),
                (1, 5, 1, 2, "https://z.test".to_string()),
            ]
        );
    }

    #[test]
    fn a_wrapped_link_has_a_run_on_each_row_it_covers() {
        let parts = markdown("aaa [bbb ccc](foam:x) ddd", 8);
        let runs = runs_of(&parts);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].0, runs[0].1, runs[0].2), (0, 4, 3));
        assert_eq!((runs[1].0, runs[1].1, runs[1].2), (1, 0, 3));
        assert!(runs.iter().all(|run| run.3 == 0));
    }

    fn style_of(text: &str, word: &str) -> Style {
        let parts = markdown(text, 60);
        let Part::Flow { rows, .. } = &parts[0] else {
            panic!("no flow");
        };
        rows.lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains(word))
            .unwrap()
            .style
    }

    #[test]
    fn a_color_link_draws_its_text_in_the_palette_colour_with_no_underline() {
        let style = style_of("[failed](color:red) ok", "failed");
        assert_eq!(style.fg, Some(palette::get().red));
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(runs_of(&markdown("[failed](color:red) ok", 60)).is_empty());
    }

    fn shown(text: &str) -> String {
        let parts = markdown(text, 60);
        let Part::Flow { rows, .. } = &parts[0] else {
            panic!("no flow");
        };
        rows.lines[0].to_string()
    }

    #[test]
    fn bold_in_a_color_link_draws_in_the_colour_and_bold_with_no_stars() {
        let text = "[**failed**](color:red) ok";
        assert_eq!(shown(text).trim_end(), "failed ok");
        let style = style_of(text, "failed");
        assert_eq!(style.fg, Some(palette::get().red));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn inline_code_in_a_color_link_draws_in_the_colour_with_no_backticks() {
        let text = "[`x`](color:red) ok";
        assert_eq!(shown(text).trim_end(), "x ok");
        assert_eq!(style_of(text, "x").fg, Some(palette::get().red));
    }

    #[test]
    fn a_color_link_in_bold_draws_in_the_colour_and_bold() {
        let text = "**[ok](color:green)** done";
        assert_eq!(shown(text).trim_end(), "ok done");
        let style = style_of(text, "ok");
        assert_eq!(style.fg, Some(palette::get().green));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_color_link_with_an_unknown_name_is_plain_text() {
        let style = style_of("[odd](color:mauve) ok", "odd");
        assert_eq!(style.fg, None);
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn a_file_link_is_underlined_in_blue() {
        let style = style_of("[ui](file:src/ui.rs#4)", "ui");
        assert_eq!(style.fg, Some(palette::get().blue));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn painting_a_hovered_link_changes_only_its_columns() {
        let mut row = Line::from(vec![Span::raw("ab "), Span::raw("cd ef")]);
        paint(&mut row, 3, 2, Color::Magenta);
        let cells: Vec<(String, Option<Color>)> = row
            .spans
            .iter()
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect();
        assert_eq!(
            cells,
            [
                ("ab ".to_string(), None),
                ("cd".to_string(), Some(Color::Magenta)),
                (" ef".to_string(), None)
            ]
        );
    }

    #[test]
    fn a_run_in_a_panned_block_keeps_only_the_columns_inside_the_window() {
        assert_eq!(panned(4, 5, 0, 10), Some((4, 5)));
        assert_eq!(panned(4, 5, 6, 10), Some((0, 3)));
        assert_eq!(panned(8, 5, 0, 10), Some((8, 2)));
        assert_eq!(panned(4, 5, 9, 10), None);
        assert_eq!(panned(12, 5, 0, 10), None);
    }

    #[test]
    fn a_table_wider_than_the_width_keeps_the_runs_of_its_links() {
        let text = "| name | note |\n|---|---|\n| [a](foam:x) | a rather long note indeed |";
        let parts = markdown(text, 12);
        let [Part::Block(KeptBlock { runs, .. })] = &parts[..] else {
            panic!("not one block");
        };
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].width, runs[0].link.target.as_str()), (1, "foam:x"));
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
        let at = |offset| {
            header_row(&row, offset, 6, &[], "#", Style::default())
                .0
                .to_string()
        };
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
    fn the_copy_glyph_measures_one_column() {
        assert_eq!(COPY_GLYPH.width(), 1);
        assert!(
            COPY_GLYPH
                .chars()
                .all(|ch| !('\u{e000}'..='\u{f8ff}').contains(&ch))
        );
    }

    #[test]
    fn a_window_over_wide_characters_is_as_many_columns_as_asked_and_never_cuts_one() {
        let row = Line::from("日本語 ab");
        for offset in 0..12 {
            for visible in 1..10 {
                let shown = window(&row, offset, visible);
                assert_eq!(shown.width(), visible, "offset {offset} visible {visible}");
            }
        }
        assert_eq!(window(&row, 1, 4).to_string(), " 本 ");
        assert_eq!(window(&row, 2, 4).to_string(), "本語");
        assert_eq!(window(&row, 6, 3).to_string(), " ab");
    }

    #[test]
    fn a_corner_label_over_a_wide_character_blanks_the_character_it_cuts() {
        let row = Line::from("日本語日本語");
        let shown = header_row(&row, 0, 6, &[], "⧉", Style::default()).0;
        assert_eq!(shown.width(), 6);
        assert_eq!(shown.to_string(), "日本 ⧉");
    }

    #[test]
    fn diff_rows_show_removed_lines_red_and_added_lines_green() {
        let rows = diff_rows(&diff_of(Some("a\nb\nc"), "a\nx\nc"), 40).lines;
        assert_eq!(plain(&rows), ["  a", "- b", "+ x", "  c"]);
        assert_eq!(rows[1].style.fg, Some(palette::get().red));
        assert_eq!(rows[2].style.fg, Some(palette::get().green));
    }

    #[test]
    fn a_new_file_shows_every_line_added() {
        let rows = diff_rows(&diff_of(None, "one\ntwo\n"), 40).lines;
        assert_eq!(plain(&rows), ["+ one", "+ two"]);
    }

    #[test]
    fn a_long_run_of_unchanged_lines_folds_into_a_count_but_a_lone_line_stays() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9";
        let rows = diff_rows(&diff_of(Some(old), "x\n2\n3\n4\n5\n6\n7\n8\ny"), 40).lines;
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
        let rows = diff_rows(&diff_of(Some("1\n2\n3\n4\n5\n6"), "x\n2\n3\n4\n5\ny"), 40).lines;
        assert_eq!(
            plain(&rows),
            ["- 1", "+ x", "  2", "  3", "  4", "  5", "- 6", "+ y"]
        );
    }

    #[test]
    fn a_diff_line_is_cut_to_the_width() {
        let rows = diff_rows(&diff_of(None, "abcdefghij"), 6).lines;
        assert_eq!(plain(&rows), ["+ abc…"]);
    }

    #[test]
    fn an_expanded_edit_shows_its_diff_indented_under_its_rows() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        edit.changed = crate::diff::size(&edit.content);
        let cwd = Path::new("/work");
        let (folded, head) = tool_view(&edit, cwd, 60, false, false, &mut Rendered::default());
        let folded = folded.lines;
        assert_eq!((folded.len(), head), (2, 2));
        let (open, head) = tool_view(&edit, cwd, 60, true, false, &mut Rendered::default());
        let open = open.lines;
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
        let (open, head) = tool_view(
            &run,
            Path::new("/work"),
            60,
            true,
            false,
            &mut Rendered::default(),
        );
        let open = open.lines;
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
        edit.changed = crate::diff::size(&edit.content);
        let (rows, _) = tool_view(
            &edit,
            Path::new("/work"),
            40,
            false,
            false,
            &mut Rendered::default(),
        );
        let rows = rows.lines;
        assert_eq!(rows[0].width(), 40);
        assert_eq!(last_cell(&rows[0]), ("▸".to_string(), dim()));
    }

    #[test]
    fn an_expanded_call_shows_the_down_marker_and_a_hovered_one_the_accent() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        edit.changed = crate::diff::size(&edit.content);
        let (rows, _) = tool_view(
            &edit,
            Path::new("/work"),
            40,
            true,
            true,
            &mut Rendered::default(),
        );
        let rows = rows.lines;
        let (glyph, style) = last_cell(&rows[0]);
        assert_eq!(glyph, "▾");
        assert_eq!(style.fg, Some(palette::get().accent));
    }

    #[test]
    fn a_subagent_call_has_the_marker_and_a_call_with_nothing_to_expand_has_none() {
        let (rows, head) = tool_view(
            &task(),
            Path::new("/work"),
            60,
            false,
            false,
            &mut Rendered::default(),
        );
        let rows = rows.lines;
        assert_eq!(head, 2);
        assert!(rows[0].to_string().ends_with('▸'));
        assert_eq!(rows[0].width(), 60);
        let read = tool("Read src/main.rs", "/work/src/main.rs", None);
        let (rows, _) = tool_view(
            &read,
            Path::new("/work"),
            60,
            false,
            false,
            &mut Rendered::default(),
        );
        let rows = rows.lines;
        assert!(!rows[0].to_string().ends_with(['▸', '▾']));
    }

    #[test]
    fn a_long_title_is_cut_before_the_marker() {
        let mut edit = tool("Edit a/very/long/path/to/some/file.rs", "/work/x", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        edit.changed = crate::diff::size(&edit.content);
        let (rows, _) = tool_view(
            &edit,
            Path::new("/work"),
            24,
            false,
            false,
            &mut Rendered::default(),
        );
        let rows = rows.lines;
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
        let lines = plain(&subagent_block(&task(), Path::new("/work"), 60, false).lines);
        assert_eq!(
            lines,
            ["● agent  Survey the entry point  1 tool", "  ⎿  running…"]
        );
    }

    #[test]
    fn an_expanded_subagent_block_draws_its_children_indented_four_columns() {
        let lines = plain(&subagent_block(&task(), Path::new("/work"), 60, true).lines);
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

    #[test]
    fn a_task_call_without_children_reads_as_an_agent_block_with_no_marker() {
        let mut replayed = task();
        replayed.children.clear();
        replayed.agent = true;
        replayed.status = ToolCallStatus::Completed;
        let (lines, head) = tool_view(
            &replayed,
            Path::new("/work"),
            60,
            false,
            false,
            &mut Rendered::default(),
        );
        let lines = lines.lines;
        let text = plain(&lines);
        assert!(text[0].starts_with("● agent"), "{text:?}");
        assert!(!text[0].contains('▸'), "{text:?}");
        assert_eq!(head, lines.len());
        assert!(!expandable(&replayed));

        replayed.agent = false;
        let (lines, _) = tool_view(
            &replayed,
            Path::new("/work"),
            60,
            false,
            false,
            &mut Rendered::default(),
        );
        let lines = lines.lines;
        assert!(plain(&lines)[0].starts_with("● think"));
    }

    #[test]
    fn an_expanded_calls_rows_are_laid_out_again_only_when_its_content_or_width_changes() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let mut rendered = Rendered::default();
        let first = rendered.expanded(&edit, 40).lines.as_ptr();
        assert_eq!(rendered.expanded(&edit, 40).lines.as_ptr(), first);
        assert_eq!(
            plain(&rendered.expanded(&edit, 40).lines),
            ["    - a", "    + b"]
        );

        edit.content = diff_of(Some("a"), "c");
        edit.revision += 1;
        assert_eq!(
            plain(&rendered.expanded(&edit, 40).lines),
            ["    - a", "    + c"]
        );
        let second = rendered.expanded(&edit, 40).lines.as_ptr();
        rendered.expanded(&edit, 30);
        assert_ne!(rendered.expanded(&edit, 30).lines.as_ptr(), second);
    }

    #[test]
    fn a_calls_head_row_holds_its_text_without_the_dot_or_the_fold_marker() {
        let mut edit = tool("Edit src/main.rs", "/work/src/main.rs", None);
        edit.kind = ToolKind::Edit;
        edit.content = diff_of(Some("a"), "b");
        let cwd = Path::new("/work");
        let (rows, _) = tool_view(&edit, cwd, 60, false, false, &mut Rendered::default());
        let head = rows.texts[0].as_ref().unwrap();
        assert_eq!(head.col, 2);
        assert_eq!(head.text, "edit   Edit src/main.rs");
    }

    #[test]
    fn a_wrapped_message_row_says_whether_it_broke_at_a_space_or_inside_a_word() {
        let rows = wrap_joined("one two abcdefghij", 7);
        let joins: Vec<cmr::Join> = rows.iter().map(|(_, join)| *join).collect();
        assert_eq!(
            joins,
            [cmr::Join::Break, cmr::Join::Space, cmr::Join::Word,]
        );
        assert_eq!(rows[1].0, "abcdefg");
    }
}
