use std::fs;
use std::path::Path;

use agent_client_protocol::v1::schema::{PlanEntryStatus, StopReason};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ChipArea, ChipTarget};
use crate::palette;
use crate::settings::{self, Category};
use crate::ui::{self, dim};

const OVERFLOW_CHIP: &str = "⋯";

const SEPARATOR: &str = " · ";

/// The frames of the working spinner.
pub const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

/// The first row of `area` for screens with no session status: `coax`, a
/// dot and `title`, then a dim rule under it.
pub fn title_bar(frame: &mut Frame, area: Rect, title: &str) {
    let [top, rule] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(identity(title))), top);
    draw_rule(frame, rule);
}

fn draw_rule(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(dim()),
        area,
    );
}

fn identity(title: &str) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled(
            "coax",
            Style::new()
                .fg(palette::get().cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(SEPARATOR, dim()),
        Span::raw(title.to_string()),
    ]
}

/// One item of the session row.
struct Item {
    spans: Vec<Span<'static>>,

    /// 0 stays when short of room. Otherwise the highest rank goes first.
    rank: u8,
}

impl Item {
    fn new(text: String, style: Style, rank: u8) -> Item {
        Item {
            spans: vec![Span::styled(text, style)],
            rank,
        }
    }

    fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }
}

fn items_width(items: &[Item]) -> usize {
    let sum: usize = items.iter().map(Item::width).sum();
    sum + SEPARATOR.width() * items.len().saturating_sub(1)
}

/// The columns the first `widths` settings take, and the `⋯` that stands for
/// the rest when `hidden`.
fn settings_width(widths: &[usize], hidden: bool) -> usize {
    let gap = SEPARATOR.width();
    let overflow = if hidden {
        gap + OVERFLOW_CHIP.width()
    } else {
        0
    };
    widths.iter().sum::<usize>() + gap * widths.len().saturating_sub(1) + overflow
}

/// How many settings fit on a row `width` columns wide, and whether a `⋯`
/// stands for the rest. Settings leave from the right, and the first stays.
fn fit_settings(widths: &[usize], width: usize) -> (usize, bool) {
    let mut shown = widths.len();
    while shown > 1 && 2 + settings_width(&widths[..shown], shown < widths.len()) > width {
        shown -= 1;
    }
    (shown, shown < widths.len())
}

/// The gaps between chips `widths` wide that spread them across a row
/// `width` columns wide with a space at each end. The first gaps take the
/// columns that do not divide evenly.
fn gaps(widths: &[usize], width: usize) -> Vec<usize> {
    let count = widths.len().saturating_sub(1);
    if count == 0 {
        return Vec::new();
    }
    let free = width.saturating_sub(2 + widths.iter().sum::<usize>());
    let (each, extra) = (free / count, free % count);
    (0..count)
        .map(|n| (each + usize::from(n < extra)).max(SEPARATOR.width()))
        .collect()
}

/// The status items left of the gap, then those right of it, with the
/// highest rank dropped first until both fit on a row `width` columns wide.
fn fit_session(mut left: Vec<Item>, mut right: Vec<Item>, width: usize) -> (Vec<Item>, Vec<Item>) {
    // a space at each end and two between the sides
    while 1 + items_width(&left) + 2 + items_width(&right) + 1 > width {
        let Some(rank) = left
            .iter()
            .chain(&right)
            .map(|item| item.rank)
            .filter(|rank| *rank > 0)
            .max()
        else {
            break;
        };
        if let Some(index) = left.iter().position(|item| item.rank == rank) {
            left.remove(index);
        } else if let Some(index) = right.iter().position(|item| item.rank == rank) {
            right.remove(index);
        }
    }
    (left, right)
}

/// The plan, the tokens and the cost.
fn session_left(app: &App) -> Vec<Item> {
    let palette = palette::get();
    let mut items = Vec::new();

    let plan = &app.transcript.plan;
    if !plan.is_empty() {
        let done = plan
            .iter()
            .filter(|entry| entry.status == PlanEntryStatus::Completed)
            .count();
        let colour = if done == plan.len() {
            palette.green
        } else {
            palette.yellow
        };
        let text = format!("plan {done}/{}", plan.len());
        items.push(Item::new(text, Style::new().fg(colour), 1));
    }
    if let Some(tokens) = &app.settings.tokens {
        let number = Style::new().fg(palette.foreground);
        let spans = vec![
            Span::styled("↑", Style::new().fg(palette.cyan)),
            Span::styled(settings::count(tokens.up), number),
            Span::styled(" ↓", Style::new().fg(palette.green)),
            Span::styled(settings::count(tokens.down), number),
        ];
        items.push(Item { spans, rank: 3 });
        if let Some(cost) = &app.settings.cost {
            let orange = Style::new().fg(palette.orange);
            items.push(Item::new(settings::money(cost), orange, 3));
        }
    }
    items
}

/// decade, the update notice and the idle state.
fn session_right(app: &App) -> Vec<Item> {
    let palette = palette::get();
    let mut items = Vec::new();
    let cyan = Style::new().fg(palette.cyan);
    match &app.decade_pane {
        Some(pane) if pane.is_empty() => items.push(Item::new("decade".to_string(), cyan, 0)),
        Some(pane) => items.push(Item::new(format!("decade pane {pane}"), cyan, 0)),
        None => {}
    }
    if let Some(version) = &app.update {
        let accent = Style::new().fg(palette.accent);
        items.push(Item::new(format!("{version} available"), accent, 4));
    }
    items.extend(state(app));
    items
}

/// The idle state, or `None` while a turn runs and the working line above
/// the prompt shows it.
fn state(app: &App) -> Option<Item> {
    if app.transcript.in_flight {
        return None;
    }
    let yellow = Style::new().fg(palette::get().yellow);
    Some(match &app.transcript.last_stop {
        Some(stop) if *stop != StopReason::EndTurn => {
            Item::new(format!("idle, {stop:?}"), yellow, 0)
        }
        _ => Item::new("idle".to_string(), dim(), 0),
    })
}

const FILLED: char = '▰';
const EMPTY: char = '▱';

/// The row above the status line: `context:`, the share of the context
/// window in use, and a bar of parallelograms across the rest of the row.
pub fn draw_context(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let line = context_line(app.settings.context.as_ref(), width);
    frame.render_widget(Paragraph::new(line), area);
}

fn context_line(context: Option<&settings::Context>, width: usize) -> Line<'static> {
    let share = context.map(|context| match context.size {
        0 => 0.0,
        size => (context.used as f64 / size as f64).min(1.0),
    });
    let percent = share.map(|share| (share * 100.0).round() as u64);
    let style = match percent {
        Some(percent) if percent > 95 => Style::new().fg(palette::get().red),
        Some(percent) if percent > 80 => Style::new().fg(palette::get().yellow),
        Some(_) => Style::new().fg(palette::get().green),
        None => dim(),
    };
    let number = percent.map_or(String::new(), |percent| format!("{percent}%"));
    let lead = format!(" context: {number}  ");
    let bar = width.saturating_sub(lead.width() + 1);
    let filled = share.map_or(0, |share| (share * bar as f64).round() as usize);
    Line::from(vec![
        Span::styled(" context: ", dim()),
        Span::styled(format!("{number}  "), style),
        Span::styled(FILLED.to_string().repeat(filled), style),
        Span::styled(EMPTY.to_string().repeat(bar - filled), dim()),
    ])
}

/// The settings row: every setting chip spread across the row, with the
/// ones that do not fit behind a `⋯`.
pub fn draw_settings(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = palette::get();
    let settings = app.settings.settings();
    let texts: Vec<String> = settings
        .iter()
        .map(|setting| setting.label.to_lowercase())
        .collect();
    let widths: Vec<usize> = texts.iter().map(|text| text.width()).collect();
    let (shown, hidden) = fit_settings(&widths, area.width as usize);
    let mut chips: Vec<(ChipTarget, &str, usize)> = (0..shown)
        .map(|index| {
            (
                ChipTarget::Setting(index),
                texts[index].as_str(),
                widths[index],
            )
        })
        .collect();
    if hidden {
        chips.push((ChipTarget::Overflow(shown), OVERFLOW_CHIP, 1));
    }
    let chip_widths: Vec<usize> = chips.iter().map(|(_, _, width)| *width).collect();
    let gaps = gaps(&chip_widths, area.width as usize);

    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    app.chips.clear();
    for (n, (target, text, width)) in chips.into_iter().enumerate() {
        if n > 0 {
            let gap = gaps[n - 1];
            let before = (gap - 1) / 2;
            spans.push(Span::raw(" ".repeat(before)));
            spans.push(Span::styled("·", dim()));
            spans.push(Span::raw(" ".repeat(gap - 1 - before)));
            x += gap as u16;
        }
        let busy_model = matches!(target, ChipTarget::Setting(index)
            if settings[index].category == Category::Model && app.transcript.in_flight);
        let style = match target {
            _ if busy_model => dim(),
            _ if app.hover == Some(target) => Style::new().fg(palette.accent),
            ChipTarget::Setting(index) => match settings[index].category {
                Category::Mode if settings[index].unguarded => Style::new().fg(palette.yellow),
                Category::Mode => Style::new().fg(palette.cyan),
                Category::Model => Style::new().fg(palette.blue),
                Category::ThoughtLevel => Style::new().fg(palette.magenta),
                Category::ModelConfig | Category::Other => dim(),
            },
            ChipTarget::Overflow(_) => dim(),
        };
        app.chips.push(ChipArea {
            area: Rect::new(x, area.y, width as u16, 1),
            target,
        });
        spans.push(Span::styled(text.to_string(), style));
        x += width as u16;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The session row: the plan, tokens and cost on the left, and decade, the
/// update notice and the idle state on the right.
pub fn draw_session(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let (left, right) = fit_session(session_left(app), session_right(app), width);
    let mut spans = vec![Span::raw(" ")];
    let gap = width.saturating_sub(2 + items_width(&left) + items_width(&right));
    push_items(&mut spans, left);
    spans.push(Span::raw(" ".repeat(gap)));
    push_items(&mut spans, right);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_items(spans: &mut Vec<Span<'static>>, items: Vec<Item>) {
    for (n, item) in items.into_iter().enumerate() {
        if n > 0 {
            spans.push(Span::styled(SEPARATOR, dim()));
        }
        spans.extend(item.spans);
    }
}

/// The place row: the agent entry, the folder and the git branch on the
/// left, and the session's title on the right.
pub fn draw_place(frame: &mut Frame, area: Rect, app: &App) {
    let folder = home_as_tilde(&app.cwd.display().to_string());
    let line = place_line(
        &app.agent,
        &folder,
        app.branch.as_deref(),
        app.settings.title.as_deref(),
        area.width as usize,
    );
    frame.render_widget(Paragraph::new(line), area);
}

fn place_line(
    agent: &str,
    folder: &str,
    branch: Option<&str>,
    title: Option<&str>,
    width: usize,
) -> Line<'static> {
    let palette = palette::get();
    let parts = [
        (agent, palette.accent),
        (folder, palette.blue),
        (branch.unwrap_or_default(), palette.green),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (text, colour) in parts.into_iter().filter(|(text, _)| !text.is_empty()) {
        if spans.len() > 1 {
            spans.push(Span::styled(SEPARATOR, dim()));
        }
        spans.push(Span::styled(text.to_string(), Style::new().fg(colour)));
    }
    let left: usize = spans.iter().map(Span::width).sum();

    // two columns between the sides and a space at the right end
    let room = width.saturating_sub(left + 2 + 1);
    if let Some(title) = title.filter(|title| !title.is_empty() && room > 1) {
        let title = ui::cut(title, room);
        spans.push(Span::raw(" ".repeat(width - left - 1 - title.width())));
        spans.push(Span::styled(title, Style::new().fg(palette.foreground)));
    }
    Line::from(spans)
}

/// `path` with the home folder at its start written `~`.
pub fn home_as_tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

/// The git branch of the repository `folder` is in, or a short commit id for
/// a detached head.
pub fn branch(folder: &Path) -> Option<String> {
    let dot_git = folder
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|path| path.exists())?;

    // a worktree's `.git` is a file naming its git folder
    let git_dir = if dot_git.is_file() {
        let text = fs::read_to_string(&dot_git).ok()?;
        dot_git
            .parent()?
            .join(text.trim().strip_prefix("gitdir: ")?)
    } else {
        dot_git
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(name) => Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_string()),
        None => Some(head.get(..7)?.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .map(|item| item.spans[0].content.as_ref())
            .collect()
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|span| span.content.clone()).collect()
    }

    const WIDTHS: [usize; 4] = [4, 4, 14, 8];

    #[test]
    fn the_context_bar_fills_the_row_and_reads_empty_before_a_size_arrives() {
        let text = |line: Line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        };
        let empty = text(context_line(None, 40));
        assert_eq!(empty, format!(" context:   {}", "▱".repeat(27)));
        let half = settings::Context {
            used: 50,
            size: 100,
        };
        let line = text(context_line(Some(&half), 40));
        assert!(line.starts_with(" context: 50%  ▰▰▰▰▰▰▰▰▰▰▰▰▱"), "{line}");
        assert_eq!(line.chars().count(), 39);
    }

    #[test]
    fn the_context_bar_is_green_then_yellow_past_80_then_red_past_95() {
        let colours = |used| {
            let context = settings::Context { used, size: 100 };
            let line = context_line(Some(&context), 40);
            let colour = |n: usize| line.spans[n].style.fg;
            (colour(1), colour(2), colour(3))
        };
        let palette = palette::get();
        let muted = dim().fg;
        assert_eq!(
            colours(50),
            (Some(palette.green), Some(palette.green), muted)
        );
        assert_eq!(
            colours(85),
            (Some(palette.yellow), Some(palette.yellow), muted)
        );
        assert_eq!(colours(97), (Some(palette.red), Some(palette.red), muted));
        let empty = context_line(None, 40);
        assert!(empty.spans.iter().all(|span| span.style == dim()));
    }

    #[test]
    fn every_setting_shows_when_they_fit_at_their_normal_spacing() {
        // 30 columns of chips, 9 of separators and a space at each end
        assert_eq!(fit_settings(&WIDTHS, 41), (4, false));
        assert_eq!(fit_settings(&WIDTHS, 200), (4, false));
    }

    #[test]
    fn settings_leave_from_the_right_into_the_overflow_mark_and_the_first_stays() {
        assert_eq!(fit_settings(&WIDTHS, 40), (3, true));
        assert_eq!(fit_settings(&WIDTHS, 28), (2, true));
        assert_eq!(fit_settings(&WIDTHS, 3), (1, true));
    }

    #[test]
    fn the_gaps_between_settings_grow_equally_to_fill_the_row() {
        // 28 free columns over three gaps
        assert_eq!(gaps(&WIDTHS, 60), [10, 9, 9]);
        assert_eq!(gaps(&WIDTHS, 41), [3, 3, 3]);
        assert_eq!(gaps(&[4], 60), [] as [usize; 0]);
    }

    fn session_items() -> (Vec<Item>, Vec<Item>) {
        let left = vec![
            Item::new("plan".to_string(), dim(), 1),
            Item::new("tokens".to_string(), dim(), 3),
            Item::new("cost".to_string(), dim(), 3),
        ];
        let right = vec![
            Item::new("decade".to_string(), dim(), 0),
            Item::new("update".to_string(), dim(), 4),
            Item::new("idle".to_string(), dim(), 0),
        ];
        (left, right)
    }

    #[test]
    fn a_narrowing_session_row_drops_the_update_then_tokens_then_cost_then_plan() {
        // the left takes 14 + 6 columns, the right 16 + 6, and 4 more for the spaces
        let fit = |width| {
            let (left, right) = session_items();
            let (left, right) = fit_session(left, right, width);
            (names(&left).join(" "), names(&right).join(" "))
        };
        let both = |left: &str, right: &str| (left.to_string(), right.to_string());
        assert_eq!(fit(46), both("plan tokens cost", "decade update idle"));
        assert_eq!(fit(45), both("plan tokens cost", "decade idle"));
        assert_eq!(fit(36), both("plan cost", "decade idle"));
        assert_eq!(fit(27), both("plan", "decade idle"));
        assert_eq!(fit(20), both("", "decade idle"));
        assert_eq!(fit(3), both("", "decade idle"));
    }

    #[test]
    fn the_place_row_shortens_the_home_folder_and_shows_the_branch() {
        let home = std::env::var("HOME").unwrap();
        let folder = home_as_tilde(&format!("{home}/projects/coax"));
        assert_eq!(folder, "~/projects/coax");
        let line = place_line("claude", &folder, Some("dev"), None, 60);
        assert_eq!(text(&line), " claude · ~/projects/coax · dev");
        let palette = palette::get();
        let colours: Vec<_> = line.spans.iter().map(|span| span.style.fg).collect();
        assert_eq!(colours[1], Some(palette.accent));
        assert_eq!(colours[3], Some(palette.blue));
        assert_eq!(colours[5], Some(palette.green));
    }

    #[test]
    fn the_place_row_sets_the_title_against_the_right_edge_and_cuts_a_long_one() {
        let line = place_line("claude", "~/coax", None, Some("first drive"), 40);
        let row = text(&line);
        assert_eq!(
            row,
            format!(" claude · ~/coax{}first drive", " ".repeat(12))
        );
        let long = "a title far too long to fit beside the rest";
        let row = text(&place_line("claude", "~/coax", None, Some(long), 40));
        assert_eq!(row.width(), 39);
        assert!(row.ends_with('…'), "{row}");
        assert!(row.contains("  a title far"), "{row}");
    }

    #[test]
    fn the_branch_is_read_from_the_head_of_the_nearest_git_folder() {
        let root = std::env::temp_dir().join(format!("coax-branch-{}", std::process::id()));
        let inner = root.join("src").join("deep");
        fs::create_dir_all(&inner).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        let head = root.join(".git").join("HEAD");
        fs::write(&head, "ref: refs/heads/feature/place\n").unwrap();
        assert_eq!(branch(&inner).as_deref(), Some("feature/place"));
        fs::write(&head, "8fee24b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6\n").unwrap();
        assert_eq!(branch(&root).as_deref(), Some("8fee24b"));
        fs::remove_dir_all(&root).unwrap();
    }
}
