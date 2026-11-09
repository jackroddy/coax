use agent_client_protocol::v1::schema::{PlanEntryStatus, StopReason};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ChipArea, ChipTarget};
use crate::palette;
use crate::settings;
use crate::ui::dim;

const CAP_LEFT: char = '\u{e0b6}';
const CAP_RIGHT: char = '\u{e0b4}';
const SOLID_RIGHT: char = '\u{e0b0}';
const SOLID_LEFT: char = '\u{e0b2}';
const THIN_RIGHT: char = '\u{e0b1}';
const THIN_LEFT: char = '\u{e0b3}';

const OVERFLOW_CHIP: &str = " ⋯ ";

/// The frames of the working spinner.
pub const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

/// A block of text on a background that the header joins to its neighbours.
pub struct Segment {
    /// The text with its padding.
    text: String,
    style: Style,
}

impl Segment {
    fn new(text: &str, style: Style) -> Segment {
        Segment {
            text: format!(" {text} "),
            style,
        }
    }

    fn bg(&self) -> Color {
        self.style.bg.unwrap_or(Color::Reset)
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// Which edge a group sits against, which sets the way its separators point.
#[derive(Clone, Copy)]
pub enum Edge {
    Left,
    Right,
}

/// The columns taken by a group of segments of these widths: a cap at each
/// end and a separator between neighbours.
fn group_width(widths: impl Iterator<Item = usize>) -> usize {
    let (count, sum) = widths.fold((0, 0), |(count, sum), width| (count + 1, sum + width));
    if count == 0 { 0 } else { sum + count + 1 }
}

/// The separator between two neighbours. Its two colours come from the
/// neighbours, so the blocks meet without a gap. Neighbours on the same
/// background get a thin line instead.
fn separator(before: &Segment, after: &Segment, edge: Edge) -> Span<'static> {
    let (solid, thin) = match edge {
        Edge::Left => (SOLID_RIGHT, THIN_RIGHT),
        Edge::Right => (SOLID_LEFT, THIN_LEFT),
    };
    if before.bg() == after.bg() {
        return Span::styled(thin.to_string(), dim().bg(before.bg()));
    }
    let style = match edge {
        Edge::Left => Style::new().fg(before.bg()).bg(after.bg()),
        Edge::Right => Style::new().fg(after.bg()).bg(before.bg()),
    };
    Span::styled(solid.to_string(), style)
}

/// `segments` as spans: a round cap, the blocks and separators, a round cap.
pub fn join(segments: &[Segment], edge: Edge) -> Vec<Span<'static>> {
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return Vec::new();
    };
    let mut spans = vec![Span::styled(
        CAP_LEFT.to_string(),
        Style::new().fg(first.bg()),
    )];
    for (n, segment) in segments.iter().enumerate() {
        if n > 0 {
            spans.push(separator(&segments[n - 1], segment, edge));
        }
        spans.push(Span::styled(segment.text.clone(), segment.style));
    }
    spans.push(Span::styled(
        CAP_RIGHT.to_string(),
        Style::new().fg(last.bg()),
    ));
    spans
}

fn badge() -> Segment {
    let style = Style::new()
        .fg(palette::INK)
        .bg(palette::get().cyan)
        .add_modifier(Modifier::BOLD);
    Segment::new("coax", style)
}

fn title_segment(title: &str) -> Segment {
    let style = Style::new()
        .fg(palette::get().foreground)
        .bg(palette::get().selection)
        .add_modifier(Modifier::BOLD);
    Segment::new(title, style)
}

/// The identity group: the coax badge and `title`.
fn identity(title: &str) -> Vec<Segment> {
    vec![badge(), title_segment(title)]
}

/// The first row of `area`: the identity group, for screens with no
/// session status to show.
pub fn title_bar(frame: &mut Frame, area: Rect, title: &str) {
    let spans = join(&identity(title), Edge::Left);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The chips that fit in `room` columns and whether a `⋯` chip stands for
/// the rest. The first chip stays.
fn fit_chips(widths: &[usize], room: usize) -> (usize, bool) {
    let overflow = OVERFLOW_CHIP.chars().count();
    let mut shown = widths.len();
    loop {
        let hidden = shown < widths.len();
        let total = group_width(
            widths[..shown]
                .iter()
                .copied()
                .chain(hidden.then_some(overflow)),
        );
        if total <= room || shown <= 1 {
            return (shown, hidden);
        }
        shown -= 1;
    }
}

/// The status segments that fit in `room` columns. A segment with a rank is
/// dropped when short of room, the highest rank first. Rank 0 stays.
fn fit_status(mut items: Vec<(u8, Segment)>, room: usize) -> Vec<Segment> {
    while group_width(items.iter().map(|(_, segment)| segment.width())) > room {
        let droppable = items
            .iter()
            .enumerate()
            .filter(|(_, (rank, _))| *rank > 0)
            .max_by_key(|(_, (rank, _))| *rank)
            .map(|(index, _)| index);
        let Some(index) = droppable else {
            break;
        };
        items.remove(index);
    }
    items.into_iter().map(|(_, segment)| segment).collect()
}

fn status(app: &App) -> Vec<(u8, Segment)> {
    let palette = palette::get();
    let quiet = dim().bg(palette.selection);
    let mut items = Vec::new();

    let plan = &app.transcript.plan;
    if !plan.is_empty() {
        let done = plan
            .iter()
            .filter(|entry| entry.status == PlanEntryStatus::Completed)
            .count();
        items.push((
            1,
            Segment::new(&format!("plan {done}/{} done", plan.len()), quiet),
        ));
    }
    if let Some(context) = &app.settings.context {
        let percent = if context.size == 0 {
            0
        } else {
            (context.used as f64 * 100.0 / context.size as f64).round() as u64
        };
        let style = if percent > 95 {
            Style::new().fg(palette.red).bg(palette.selection)
        } else if percent > 80 {
            Style::new().fg(palette.yellow).bg(palette.selection)
        } else {
            quiet
        };
        let text = format!(
            "ctx {}/{} {percent}%",
            settings::count(context.used),
            settings::count(context.size)
        );
        items.push((2, Segment::new(&text, style)));
    }
    if let Some(tokens) = &app.settings.tokens {
        let mut text = format!(
            "↑{} ↓{}",
            settings::count(tokens.up),
            settings::count(tokens.down)
        );
        if let Some(cost) = &app.settings.cost {
            text.push(' ');
            text.push_str(&settings::money(cost));
        }
        items.push((3, Segment::new(&text, quiet)));
    }
    match &app.decade_pane {
        Some(pane) if pane.is_empty() => items.push((0, Segment::new("decade", quiet))),
        Some(pane) => items.push((0, Segment::new(&format!("decade pane {pane}"), quiet))),
        None => {}
    }
    items.push((0, state(app)));
    items
}

fn state(app: &App) -> Segment {
    let palette = palette::get();
    if app.transcript.in_flight {
        let spinner = SPINNER[app.spinner % SPINNER.len()];
        let style = Style::new().fg(palette::INK).bg(palette.yellow);
        return Segment::new(&format!("{spinner} working"), style);
    }
    match &app.transcript.last_stop {
        Some(stop) if *stop != StopReason::EndTurn => Segment::new(
            &format!("idle, {stop:?}"),
            Style::new().fg(palette.yellow).bg(palette.selection),
        ),
        _ => Segment::new("idle", dim().bg(palette.selection)),
    }
}

/// The two header rows: identity and session status on the first, the
/// setting chips on the second.
pub fn draw_header(frame: &mut Frame, area: Rect, app: &mut App) {
    let [top, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let width = area.width as usize;

    let left = join(&identity(&app.agent), Edge::Left);
    let left_width: usize = left.iter().map(Span::width).sum();
    let right = fit_status(status(app), width.saturating_sub(left_width + 1));
    let right = join(&right, Edge::Right);
    let right_width: usize = right.iter().map(Span::width).sum();
    let gap = width.saturating_sub(left_width + right_width);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    frame.render_widget(Paragraph::new(Line::from(spans)), top);

    draw_chips(frame, bottom, app);
}

fn draw_chips(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = palette::get();
    let settings = app.settings.settings();
    let labels: Vec<Segment> = settings
        .iter()
        .map(|setting| Segment::new(&setting.label, Style::new()))
        .collect();
    let widths: Vec<usize> = labels.iter().map(Segment::width).collect();
    let (shown, hidden) = fit_chips(&widths, area.width as usize);

    let base = Style::new().fg(palette.foreground).bg(palette.selection);
    let mut segments: Vec<Segment> = Vec::new();
    let mut targets: Vec<ChipTarget> = Vec::new();
    for (index, setting) in settings.iter().enumerate().take(shown) {
        let disabled = setting.model && app.transcript.in_flight;
        let target = ChipTarget::Setting(index);
        let mut style = base;
        if setting.unguarded {
            style = Style::new()
                .fg(palette::INK)
                .bg(palette.yellow)
                .add_modifier(Modifier::BOLD);
        }
        if disabled {
            style = dim().bg(palette.selection);
        } else if app.hover == Some(target) {
            style = style.fg(palette.accent);
        }
        segments.push(Segment {
            text: labels[index].text.clone(),
            style,
        });
        targets.push(target);
    }
    if hidden {
        let target = ChipTarget::Overflow(shown);
        let style = if app.hover == Some(target) {
            base.fg(palette.accent)
        } else {
            base
        };
        segments.push(Segment {
            text: OVERFLOW_CHIP.to_string(),
            style,
        });
        targets.push(target);
    }

    app.chips.clear();
    let mut x = area.x + 1;
    for (segment, target) in segments.iter().zip(targets) {
        let chip_width = segment.width() as u16;
        app.chips.push(ChipArea {
            area: Rect::new(x, area.y, chip_width, 1),
            target,
        });
        x += chip_width + 1;
    }
    frame.render_widget(
        Paragraph::new(Line::from(join(&segments, Edge::Left))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, bg: Color) -> Segment {
        Segment::new(text, Style::new().fg(Color::White).bg(bg))
    }

    #[test]
    fn a_group_is_capped_at_both_ends_in_the_colour_of_its_end_blocks() {
        let spans = join(
            &[segment("a", Color::Red), segment("b", Color::Blue)],
            Edge::Left,
        );
        let first = spans.first().unwrap();
        let last = spans.last().unwrap();
        assert_eq!(first.content, CAP_LEFT.to_string());
        assert_eq!(first.style, Style::new().fg(Color::Red));
        assert_eq!(last.content, CAP_RIGHT.to_string());
        assert_eq!(last.style, Style::new().fg(Color::Blue));
    }

    #[test]
    fn a_left_group_separator_takes_its_colours_from_the_neighbours() {
        let spans = join(
            &[segment("a", Color::Red), segment("b", Color::Blue)],
            Edge::Left,
        );
        let between = &spans[2];
        assert_eq!(between.content, SOLID_RIGHT.to_string());
        assert_eq!(between.style.fg, Some(Color::Red));
        assert_eq!(between.style.bg, Some(Color::Blue));
    }

    #[test]
    fn a_right_group_separator_swaps_the_colours_to_face_the_other_way() {
        let spans = join(
            &[segment("a", Color::Red), segment("b", Color::Blue)],
            Edge::Right,
        );
        let between = &spans[2];
        assert_eq!(between.content, SOLID_LEFT.to_string());
        assert_eq!(between.style.fg, Some(Color::Blue));
        assert_eq!(between.style.bg, Some(Color::Red));
    }

    #[test]
    fn neighbours_on_one_background_are_split_by_a_thin_line_on_that_background() {
        let spans = join(
            &[segment("a", Color::Red), segment("b", Color::Red)],
            Edge::Left,
        );
        let between = &spans[2];
        assert_eq!(between.content, THIN_RIGHT.to_string());
        assert_eq!(between.style.bg, Some(Color::Red));
    }

    #[test]
    fn a_group_is_as_wide_as_its_blocks_plus_caps_and_separators() {
        let segments = [segment("a", Color::Red), segment("bb", Color::Blue)];
        let spans = join(&segments, Edge::Left);
        let drawn: usize = spans.iter().map(Span::width).sum();
        assert_eq!(drawn, group_width(segments.iter().map(Segment::width)));
        assert_eq!(drawn, 3 + 4 + 3);
        assert_eq!(group_width(std::iter::empty()), 0);
    }

    #[test]
    fn every_chip_shows_when_there_is_room() {
        assert_eq!(fit_chips(&[6, 8, 10, 9], 60), (4, false));
    }

    #[test]
    fn chips_leave_from_the_right_into_the_overflow_chip() {
        // 4 chips take 6 + 8 + 10 + 9 + 3 + 2 = 38 columns
        assert_eq!(fit_chips(&[6, 8, 10, 9], 38), (4, false));
        // three chips and the overflow chip take 6 + 8 + 10 + 3 + 3 + 2 = 32
        assert_eq!(fit_chips(&[6, 8, 10, 9], 37), (3, true));
        assert_eq!(fit_chips(&[6, 8, 10, 9], 32), (3, true));
        assert_eq!(fit_chips(&[6, 8, 10, 9], 31), (2, true));
    }

    #[test]
    fn the_first_chip_stays_however_narrow_the_header_gets() {
        assert_eq!(fit_chips(&[6, 8, 10, 9], 3), (1, true));
    }

    fn items() -> Vec<(u8, Segment)> {
        vec![
            (1, segment("plan", Color::Red)),
            (2, segment("context", Color::Red)),
            (3, segment("tokens", Color::Red)),
            (0, segment("idle", Color::Red)),
        ]
    }

    fn names(segments: &[Segment]) -> Vec<&str> {
        segments.iter().map(|s| s.text.trim()).collect()
    }

    #[test]
    fn a_narrowing_status_group_drops_tokens_then_context_then_plan() {
        // widths 6 + 9 + 8 + 6, three separators and two caps
        assert_eq!(
            names(&fit_status(items(), 100)),
            ["plan", "context", "tokens", "idle"]
        );
        assert_eq!(names(&fit_status(items(), 28)), ["plan", "context", "idle"]);
        assert_eq!(names(&fit_status(items(), 18)), ["plan", "idle"]);
        assert_eq!(names(&fit_status(items(), 10)), ["idle"]);
        assert_eq!(names(&fit_status(items(), 1)), ["idle"]);
    }
}
