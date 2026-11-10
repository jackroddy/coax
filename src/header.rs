use agent_client_protocol::v1::schema::{PlanEntryStatus, StopReason};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ChipArea, ChipTarget};
use crate::palette;
use crate::settings;
use crate::ui::dim;

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
        self.text.width()
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

/// What fits on a line `width` columns wide: how many settings, whether a `⋯`
/// stands for the rest, and the status items kept. The settings past the
/// first `steady` go first, then the items, the highest rank first, then the
/// rest of the settings from the right. The first setting and the rank 0
/// items stay.
fn fit_line(
    widths: &[usize],
    steady: usize,
    mut items: Vec<Item>,
    width: usize,
) -> (usize, bool, Vec<Item>) {
    let mut shown = widths.len();
    loop {
        let hidden = shown < widths.len();

        // a space at each end and two between the sides
        let total = 1 + settings_width(&widths[..shown], hidden) + 2 + items_width(&items) + 1;
        if total <= width {
            return (shown, hidden, items);
        }
        let droppable = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.rank > 0)
            .max_by_key(|(_, item)| item.rank)
            .map(|(index, _)| index);
        if shown > steady.max(1) {
            shown -= 1;
        } else if let Some(index) = droppable {
            items.remove(index);
        } else if shown > 1 {
            shown -= 1;
        } else {
            return (shown, hidden, items);
        }
    }
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
    if let Some(version) = &app.update {
        items.push(Item::new(format!("{version} available"), dim(), 4));
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

/// The status line: the settings on the left and the session status on the
/// right, all dim but the mode value when the agent does not ask and a turn
/// that stopped for a reason other than its end.
pub fn draw_status(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = palette::get();
    let settings = app.settings.settings();
    let texts: Vec<String> = settings
        .iter()
        .map(|setting| setting.label.to_lowercase())
        .collect();
    let widths: Vec<usize> = texts.iter().map(|text| text.width()).collect();
    let steady = settings
        .iter()
        .filter(|setting| !setting.rarely_changed)
        .count();
    let (shown, hidden, items) = fit_line(&widths, steady, status(app), area.width as usize);

    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    app.chips.clear();
    let mut place = |spans: &mut Vec<Span<'static>>, width: usize, target: ChipTarget| {
        if spans.len() > 1 {
            spans.push(Span::styled(SEPARATOR, dim()));
            x += SEPARATOR.width() as u16;
        }
        app.chips.push(ChipArea {
            area: Rect::new(x, area.y, width as u16, 1),
            target,
        });
        x += width as u16;
    };
    for (index, setting) in settings.iter().enumerate().take(shown) {
        let target = ChipTarget::Setting(index);
        let mut style = dim();
        if setting.unguarded {
            style = Style::new().fg(palette.yellow);
        }
        if setting.model && app.transcript.in_flight {
            style = dim();
        } else if app.hover == Some(target) {
            style = Style::new().fg(palette.accent);
        }
        place(&mut spans, widths[index], target);
        spans.push(Span::styled(texts[index].clone(), style));
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

    let left_width: usize = spans.iter().map(Span::width).sum();
    let right_width = items_width(&items) + 1;
    spans.push(Span::raw(" ".repeat(
        (area.width as usize).saturating_sub(left_width + right_width),
    )));
    for (n, item) in items.into_iter().enumerate() {
        if n > 0 {
            spans.push(Span::styled(SEPARATOR, dim()));
        }
        spans.push(Span::styled(item.text, item.style));
    }
    spans.push(Span::raw(" "));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const WIDTHS: [usize; 4] = [4, 4, 14, 8];

    #[test]
    fn a_wide_line_shows_every_setting_and_every_item() {
        let (shown, hidden, kept) = fit_line(&WIDTHS, 4, items(), 200);
        assert_eq!((shown, hidden), (4, false));
        assert_eq!(names(&kept), ["plan", "context", "tokens", "idle"]);
    }

    #[test]
    fn a_narrowing_line_drops_tokens_then_context_then_plan_then_settings_from_the_right() {
        // settings take 30 + 9 columns, items 21 + 9, and 4 more for the spaces
        let fit = |width| {
            let (shown, hidden, kept) = fit_line(&WIDTHS, 4, items(), width);
            (shown, hidden, kept.len())
        };
        assert_eq!(fit(73), (4, false, 4));
        assert_eq!(fit(72), (4, false, 3));
        assert_eq!(fit(58), (4, false, 2));
        assert_eq!(fit(51), (4, false, 1));
    }

    #[test]
    fn once_the_items_are_down_to_the_state_settings_leave_into_the_overflow_mark() {
        let (shown, hidden, kept) = fit_line(&WIDTHS, 4, items(), 44);
        assert_eq!((shown, hidden), (3, true));
        assert_eq!(names(&kept), ["idle"]);
        let (shown, hidden, _) = fit_line(&WIDTHS, 4, items(), 30);
        assert_eq!((shown, hidden), (2, true));
    }

    #[test]
    fn rarely_changed_settings_leave_into_the_overflow_mark_before_any_item_drops() {
        let (shown, hidden, kept) = fit_line(&WIDTHS, 2, items(), 72);
        assert_eq!((shown, hidden), (3, true));
        assert_eq!(names(&kept), ["plan", "context", "tokens", "idle"]);
        let (shown, hidden, kept) = fit_line(&WIDTHS, 2, items(), 58);
        assert_eq!((shown, hidden), (2, true));
        assert_eq!(names(&kept), ["plan", "context", "tokens", "idle"]);
    }

    #[test]
    fn the_first_setting_and_the_state_stay_however_narrow_the_line_gets() {
        let (shown, hidden, kept) = fit_line(&WIDTHS, 4, items(), 3);
        assert_eq!((shown, hidden), (1, true));
        assert_eq!(names(&kept), ["idle"]);
    }
}
