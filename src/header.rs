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

/// The powerline arrow that points left.
const ARROW: &str = "\u{e0b2}";

const OVERFLOW_CHIP: &str = " ⋯ ";
const SEPARATOR: &str = " · ";

/// The frames of the working spinner.
pub const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

/// A chip of the strip: a badge on `colour`, which an arrow in `colour`
/// leads into.
pub struct Chip {
    /// The text with one space of padding each side.
    text: String,
    colour: Color,
    style: Style,
}

impl Chip {
    fn new(text: &str, colour: Color) -> Chip {
        let style = Style::new()
            .bg(colour)
            .fg(palette::INK)
            .add_modifier(Modifier::BOLD);
        Chip {
            text: format!(" {text} "),
            colour,
            style,
        }
    }

    /// The columns the arrow and the badge take.
    fn width(&self) -> usize {
        1 + self.text.chars().count()
    }
}

/// `chips` flowing into one another: each arrow takes the chip's colour
/// with the previous chip's colour behind it, and the terminal's own
/// background behind the first.
pub fn flow(chips: &[Chip]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut previous = None;
    for chip in chips {
        let mut arrow = Style::new().fg(chip.colour);
        if let Some(previous) = previous {
            arrow = arrow.bg(previous);
        }
        spans.push(Span::styled(ARROW, arrow));
        spans.push(Span::styled(chip.text.clone(), chip.style));
        previous = Some(chip.colour);
    }
    spans
}

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

/// The chips that fit in `room` columns and whether a `⋯` chip stands for
/// the rest. The first chip stays.
fn fit_chips(widths: &[usize], room: usize) -> (usize, bool) {
    let overflow = 1 + OVERFLOW_CHIP.chars().count();
    let mut shown = widths.len();
    loop {
        let hidden = shown < widths.len();
        let total: usize =
            widths[..shown].iter().sum::<usize>() + if hidden { overflow } else { 0 };
        if total <= room || shown <= 1 {
            return (shown, hidden);
        }
        shown -= 1;
    }
}

/// One item of the status side of line 1.
struct Item {
    text: String,
    style: Style,

    /// 0 stays when short of room. Otherwise the highest rank goes first.
    rank: u8,
}

impl Item {
    fn new(text: String, style: Style, rank: u8) -> Item {
        Item { text, style, rank }
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

fn items_width(items: &[Item]) -> usize {
    let sum: usize = items.iter().map(Item::width).sum();
    sum + SEPARATOR.chars().count() * items.len().saturating_sub(1)
}

/// The items that fit in `room` columns.
fn fit_status(mut items: Vec<Item>, room: usize) -> Vec<Item> {
    while items_width(&items) > room {
        let droppable = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.rank > 0)
            .max_by_key(|(_, item)| item.rank)
            .map(|(index, _)| index);
        let Some(index) = droppable else {
            break;
        };
        items.remove(index);
    }
    items
}

fn status(app: &App) -> Vec<Item> {
    let palette = palette::get();
    let mut items = Vec::new();

    let plan = &app.transcript.plan;
    if !plan.is_empty() {
        let done = plan
            .iter()
            .filter(|entry| entry.status == PlanEntryStatus::Completed)
            .count();
        items.push(Item::new(format!("plan {done}/{}", plan.len()), dim(), 1));
    }
    if let Some(context) = &app.settings.context {
        let percent = if context.size == 0 {
            0
        } else {
            (context.used as f64 * 100.0 / context.size as f64).round() as u64
        };
        let style = if percent > 95 {
            Style::new().fg(palette.red)
        } else if percent > 80 {
            Style::new().fg(palette.yellow)
        } else {
            dim()
        };
        let text = format!(
            "ctx {}/{} {percent}%",
            settings::count(context.used),
            settings::count(context.size)
        );
        items.push(Item::new(text, style, 2));
    }
    if let Some(tokens) = &app.settings.tokens {
        let text = format!(
            "↑{} ↓{}",
            settings::count(tokens.up),
            settings::count(tokens.down)
        );
        items.push(Item::new(text, dim(), 3));
        if let Some(cost) = &app.settings.cost {
            items.push(Item::new(settings::money(cost), dim(), 3));
        }
    }
    match &app.decade_pane {
        Some(pane) if pane.is_empty() => items.push(Item::new("decade".to_string(), dim(), 0)),
        Some(pane) => items.push(Item::new(format!("decade pane {pane}"), dim(), 0)),
        None => {}
    }
    items.push(state(app));
    items
}

fn state(app: &App) -> Item {
    let yellow = Style::new().fg(palette::get().yellow);
    if app.transcript.in_flight {
        let spinner = SPINNER[app.spinner % SPINNER.len()];
        return Item::new(format!("{spinner} working"), yellow, 0);
    }
    match &app.transcript.last_stop {
        Some(stop) if *stop != StopReason::EndTurn => {
            Item::new(format!("idle, {stop:?}"), yellow, 0)
        }
        _ => Item::new("idle".to_string(), dim(), 0),
    }
}

/// The header's three rows: `coax`, the agent and the session status; the
/// setting chips; a rule.
pub fn draw_header(frame: &mut Frame, area: Rect, app: &mut App) {
    let [top, strip, rule] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
    let width = area.width as usize;

    let mut spans = identity(&app.agent);
    let left_width: usize = spans.iter().map(Span::width).sum();

    // a column of padding after the status, as before it
    let room = width.saturating_sub(left_width + 2);
    let items = fit_status(status(app), room);
    let right_width = items_width(&items) + 1;
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(left_width + right_width)),
    ));
    for (n, item) in items.into_iter().enumerate() {
        if n > 0 {
            spans.push(Span::styled(SEPARATOR, dim()));
        }
        spans.push(Span::styled(item.text, item.style));
    }
    spans.push(Span::raw(" "));
    frame.render_widget(Paragraph::new(Line::from(spans)), top);

    draw_chips(frame, strip, app);
    draw_rule(frame, rule);
}

/// The colour of the chip at `index` among the settings. The first three are
/// the mode, the model and the thought level, and the rest cycle through
/// three more.
fn chip_colour(index: usize, unguarded: bool) -> Color {
    let palette = palette::get();
    match index {
        0 if unguarded => palette.yellow,
        0 => palette.green,
        1 => palette.magenta,
        2 => palette.blue,
        _ => [
            palette.cyan,
            palette.orange,
            palette.muted.unwrap_or(Color::DarkGray),
        ][(index - 3) % 3],
    }
}

fn draw_chips(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = palette::get();
    let settings = app.settings.settings();
    let widths: Vec<usize> = settings
        .iter()
        .map(|setting| Chip::new(&setting.label, Color::Reset).width())
        .collect();
    let (shown, hidden) = fit_chips(&widths, area.width as usize);

    let mut chips: Vec<Chip> = Vec::new();
    let mut targets: Vec<ChipTarget> = Vec::new();
    for (index, setting) in settings.iter().enumerate().take(shown) {
        let target = ChipTarget::Setting(index);
        let mut chip = Chip::new(&setting.label, chip_colour(index, setting.unguarded));
        if setting.model && app.transcript.in_flight {
            chip.style = chip
                .style
                .remove_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM);
        } else if app.hover == Some(target) {
            chip.style = chip.style.fg(palette.foreground);
        }
        chips.push(chip);
        targets.push(target);
    }
    if hidden {
        let target = ChipTarget::Overflow(shown);
        let previous = chips.last().map(|chip| chip.colour);
        let candidates = [palette.muted.unwrap_or(Color::DarkGray), palette.cyan];
        let colour = candidates
            .into_iter()
            .find(|colour| Some(*colour) != previous)
            .unwrap_or(Color::DarkGray);
        let mut chip = Chip::new(OVERFLOW_CHIP.trim(), colour);
        if app.hover == Some(target) {
            chip.style = chip.style.fg(palette.foreground);
        }
        chips.push(chip);
        targets.push(target);
    }

    // the strip sits against the right edge
    let total: usize = chips.iter().map(Chip::width).sum();
    let mut x = area.right().saturating_sub(total as u16);
    app.chips.clear();
    for (chip, target) in chips.iter().zip(targets) {
        // the arrow is not part of the click area
        let text_width = chip.width() as u16 - 1;
        app.chips.push(ChipArea {
            area: Rect::new(x + 1, area.y, text_width, 1),
            target,
        });
        x += chip.width() as u16;
    }
    frame.render_widget(
        Paragraph::new(Line::from(flow(&chips))).right_aligned(),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(text: &str, colour: Color) -> Chip {
        Chip::new(text, colour)
    }

    #[test]
    fn a_chip_is_black_bold_text_on_its_colour_with_a_space_each_side() {
        let spans = flow(&[chip("Auto", Color::Red)]);
        assert_eq!(spans[1].content, " Auto ");
        assert_eq!(spans[1].style.bg, Some(Color::Red));
        assert_eq!(spans[1].style.fg, Some(palette::INK));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_first_arrow_has_the_terminals_background_behind_it() {
        let spans = flow(&[chip("a", Color::Red)]);
        assert_eq!(spans[0].content, ARROW);
        assert_eq!(spans[0].style, Style::new().fg(Color::Red));
    }

    #[test]
    fn an_arrow_between_two_chips_is_the_second_colour_on_the_first() {
        let spans = flow(&[chip("a", Color::Red), chip("b", Color::Blue)]);
        assert_eq!(spans[2].content, ARROW);
        assert_eq!(spans[2].style.fg, Some(Color::Blue));
        assert_eq!(spans[2].style.bg, Some(Color::Red));
    }

    #[test]
    fn no_round_cap_is_drawn() {
        let spans = flow(&[chip("a", Color::Red), chip("b", Color::Blue)]);
        assert!(
            spans
                .iter()
                .all(|span| !span.content.contains(['\u{e0b4}', '\u{e0b6}']))
        );
    }

    #[test]
    fn the_strip_is_as_wide_as_its_chips_each_with_its_arrow() {
        let chips = [chip("a", Color::Red), chip("bb", Color::Blue)];
        let drawn: usize = flow(&chips).iter().map(Span::width).sum();
        assert_eq!(drawn, 4 + 5);
        assert_eq!(drawn, chips.iter().map(Chip::width).sum::<usize>());
    }

    #[test]
    fn every_chip_shows_when_there_is_room() {
        assert_eq!(fit_chips(&[7, 7, 17, 11], 60), (4, false));
    }

    #[test]
    fn chips_leave_from_the_right_into_the_overflow_chip() {
        // four chips take 7 + 7 + 17 + 11 = 42 columns
        assert_eq!(fit_chips(&[7, 7, 17, 11], 42), (4, false));
        // three chips and the overflow chip take 31 + 4 = 35
        assert_eq!(fit_chips(&[7, 7, 17, 11], 41), (3, true));
        assert_eq!(fit_chips(&[7, 7, 17, 11], 35), (3, true));
        assert_eq!(fit_chips(&[7, 7, 17, 11], 34), (2, true));
    }

    #[test]
    fn the_first_chip_stays_however_narrow_the_header_gets() {
        assert_eq!(fit_chips(&[7, 7, 17, 11], 3), (1, true));
    }

    #[test]
    fn neighbouring_chips_have_different_colours() {
        let colours: Vec<Color> = (0..8).map(|index| chip_colour(index, false)).collect();
        for pair in colours.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
    }

    #[test]
    fn the_mode_chip_is_yellow_when_the_agent_does_not_ask_and_green_otherwise() {
        assert_eq!(chip_colour(0, true), palette::get().yellow);
        assert_eq!(chip_colour(0, false), palette::get().green);
    }

    fn items() -> Vec<Item> {
        vec![
            Item::new("plan".to_string(), dim(), 1),
            Item::new("context".to_string(), dim(), 2),
            Item::new("tokens".to_string(), dim(), 3),
            Item::new("idle".to_string(), dim(), 0),
        ]
    }

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|item| item.text.as_str()).collect()
    }

    #[test]
    fn a_narrowing_status_drops_tokens_then_context_then_plan() {
        // 4 + 7 + 6 + 4 columns and three separators of 3
        assert_eq!(
            names(&fit_status(items(), 100)),
            ["plan", "context", "tokens", "idle"]
        );
        assert_eq!(names(&fit_status(items(), 29)), ["plan", "context", "idle"]);
        assert_eq!(names(&fit_status(items(), 17)), ["plan", "idle"]);
        assert_eq!(names(&fit_status(items(), 8)), ["idle"]);
        assert_eq!(names(&fit_status(items(), 1)), ["idle"]);
    }
}
