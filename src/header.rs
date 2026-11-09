use agent_client_protocol::v1::schema::{PlanEntryStatus, StopReason};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ChipArea, ChipTarget};
use crate::palette;
use crate::settings;
use crate::ui::dim;

const OVERFLOW_CHIP: &str = "⋯";

/// The columns between two settings.
const SETTING_GAP: &str = "   ";
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

/// The settings that fit in `room` columns after the leading space, and
/// whether a `⋯` stands for the rest. The first setting stays.
fn fit_chips(widths: &[usize], room: usize) -> (usize, bool) {
    let gap = SETTING_GAP.chars().count();
    let overflow = gap + OVERFLOW_CHIP.chars().count();
    let mut shown = widths.len();
    loop {
        let hidden = shown < widths.len();
        let total = widths[..shown].iter().sum::<usize>()
            + gap * shown.saturating_sub(1)
            + if hidden { overflow } else { 0 };
        if total < room || shown <= 1 {
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

/// A setting as the header's second line shows it: its name dim and its
/// value after it.
fn setting_text(name: &str, value: &str) -> (String, String) {
    (name.to_lowercase(), value.to_lowercase())
}

fn draw_chips(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = palette::get();
    let settings = app.settings.settings();
    let texts: Vec<(String, String)> = settings
        .iter()
        .map(|setting| setting_text(&setting.name, &setting.value))
        .collect();
    let widths: Vec<usize> = texts
        .iter()
        .map(|(name, value)| name.chars().count() + 1 + value.chars().count())
        .collect();
    let (shown, hidden) = fit_chips(&widths, area.width as usize);

    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    app.chips.clear();
    let mut place = |spans: &mut Vec<Span<'static>>, width: usize, target: ChipTarget| {
        if spans.len() > 1 {
            spans.push(Span::raw(SETTING_GAP));
            x += SETTING_GAP.len() as u16;
        }
        app.chips.push(ChipArea {
            area: Rect::new(x, area.y, width as u16, 1),
            target,
        });
        x += width as u16;
    };
    for (index, setting) in settings.iter().enumerate().take(shown) {
        let target = ChipTarget::Setting(index);
        let (name, value) = &texts[index];
        let mut style = Style::default();
        if setting.unguarded {
            style = Style::new().fg(palette.yellow);
        }
        if setting.model && app.transcript.in_flight {
            style = dim();
        } else if app.hover == Some(target) {
            style = Style::new().fg(palette.accent);
        }
        place(&mut spans, widths[index], target);
        spans.push(Span::styled(format!("{name} "), dim()));
        spans.push(Span::styled(value.clone(), style));
    }
    if hidden {
        let target = ChipTarget::Overflow(shown);
        let style = if app.hover == Some(target) {
            Style::new().fg(palette.accent)
        } else {
            dim()
        };
        place(&mut spans, 1, target);
        spans.push(Span::styled(OVERFLOW_CHIP, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_shows_when_there_is_room() {
        assert_eq!(fit_chips(&[8, 10, 14, 8], 120), (4, false));
    }

    #[test]
    fn settings_leave_from_the_right_into_the_overflow_mark() {
        // four settings take 1 + 40 + 3 * 3 = 50 columns
        assert_eq!(fit_chips(&[8, 10, 14, 8], 50), (4, false));
        // three settings and the mark take 1 + 32 + 6 + 4 = 43
        assert_eq!(fit_chips(&[8, 10, 14, 8], 49), (3, true));
        assert_eq!(fit_chips(&[8, 10, 14, 8], 43), (3, true));
        assert_eq!(fit_chips(&[8, 10, 14, 8], 42), (2, true));
    }

    #[test]
    fn the_first_setting_stays_however_narrow_the_header_gets() {
        assert_eq!(fit_chips(&[8, 10, 14, 8], 3), (1, true));
    }

    #[test]
    fn a_setting_shows_its_name_and_value_in_lowercase() {
        assert_eq!(
            setting_text("Effort", "Default"),
            ("effort".to_string(), "default".to_string())
        );
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
