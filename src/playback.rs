use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Position;

use serde_json::json;
use unicode_width::UnicodeWidthStr;

use crate::log;
use crate::scenario::{Anchor, InputStep, Target, Wait};

const PER_CHAR: Duration = Duration::from_millis(50);
const RECHECK: Duration = Duration::from_millis(50);
const GLIDE: Duration = Duration::from_millis(500);

/// How long a mouse step waits for its text to reach the screen before the
/// playback stops.
const LOST_FOR: Duration = Duration::from_secs(3);

/// A scenario's `input`, played into coax as key presses and mouse events.
#[derive(Default)]
pub struct Player {
    steps: VecDeque<InputStep>,
    queued: VecDeque<Event>,
    interval: Duration,
    at: Option<Instant>,
    pointer: Option<Position>,
    lost_since: Option<Instant>,
}

impl Player {
    /// # Errors
    ///
    /// A `key` step that names no key.
    pub fn new(steps: Vec<InputStep>) -> Result<Player, String> {
        for step in &steps {
            if let InputStep::Key { key } = step
                && key_event(key).is_none()
            {
                return Err(format!("input names no key {key}"));
            }
        }
        Ok(Player {
            steps: steps.into(),
            interval: PER_CHAR,
            ..Player::default()
        })
    }

    /// When `next` has something to do, or `None` once every step has played.
    pub fn due(&self) -> Option<Instant> {
        if self.steps.is_empty() && self.queued.is_empty() {
            return None;
        }
        Some(self.at.unwrap_or_else(Instant::now))
    }

    /// The next event to play at `now`, if one is due. `idle` is whether the
    /// agent's turn has ended, and `screen` is the frame coax last drew.
    pub fn next(&mut self, now: Instant, idle: bool, screen: &Buffer) -> Option<Event> {
        while self.at.is_none_or(|at| at <= now) {
            if let Some(event) = self.queued.pop_front() {
                if let Event::Mouse(mouse) = event {
                    self.pointer = Some(Position::new(mouse.column, mouse.row));
                }
                self.at = Some(now + self.interval);
                return Some(event);
            }
            let step = self.steps.pop_front()?;
            match step {
                InputStep::Type {
                    r#type,
                    per_char_ms,
                } => {
                    self.interval = per_char_ms.map_or(PER_CHAR, Duration::from_millis);
                    self.queued = r#type
                        .chars()
                        .map(|ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)))
                        .collect();
                }
                InputStep::Key { key } => self.queued.extend(key_event(&key).map(Event::Key)),
                InputStep::Delay { delay_ms } => {
                    self.at = Some(now + Duration::from_millis(delay_ms));
                }
                InputStep::Wait { wait: Wait::Idle } if !idle => {
                    self.steps.push_front(step);
                    self.at = Some(now + RECHECK);
                }
                InputStep::Wait { .. } => {}
                mouse => match self.plan(&mouse, screen) {
                    Some((events, glide)) => {
                        self.lost_since = None;
                        self.interval = glide / events.len().max(1) as u32;
                        self.queued = events;
                    }
                    None if now - *self.lost_since.get_or_insert(now) > LOST_FOR => {
                        log::info(
                            "error",
                            json!({"text": format!("input lost its target in {mouse:?}")}),
                        );
                        self.steps.clear();
                        return None;
                    }
                    None => {
                        self.steps.push_front(mouse);
                        self.at = Some(now + RECHECK);
                    }
                },
            }
        }
        None
    }

    /// The events of a mouse step and how long they take, or `None` while a
    /// target is not on `screen`.
    fn plan(&self, step: &InputStep, screen: &Buffer) -> Option<(VecDeque<Event>, Duration)> {
        let glide = |ms: &Option<u64>| ms.map_or(GLIDE, Duration::from_millis);
        let mut events = VecDeque::new();
        let duration = match step {
            InputStep::Move { r#move, ms } => {
                let to = find(screen, r#move)?;
                self.glide(to, MouseEventKind::Moved, &mut events);
                glide(ms)
            }
            InputStep::Click { click: target, ms } | InputStep::Double { double: target, ms } => {
                let to = find(screen, target)?;
                self.glide(to, MouseEventKind::Moved, &mut events);
                let clicks = if matches!(step, InputStep::Double { .. }) {
                    2
                } else {
                    1
                };
                for _ in 0..clicks {
                    events.push_back(mouse(MouseEventKind::Down(MouseButton::Left), to));
                    events.push_back(mouse(MouseEventKind::Up(MouseButton::Left), to));
                }
                glide(ms)
            }
            InputStep::Drag { drag, ms } => {
                let from = find(screen, &drag.from)?;
                let to = find(screen, &drag.to)?;
                self.glide(from, MouseEventKind::Moved, &mut events);
                events.push_back(mouse(MouseEventKind::Down(MouseButton::Left), from));
                line(
                    from,
                    to,
                    MouseEventKind::Drag(MouseButton::Left),
                    &mut events,
                );
                events.push_back(mouse(MouseEventKind::Up(MouseButton::Left), to));
                glide(ms)
            }
            _ => return None,
        };
        Some((events, duration))
    }

    /// Moves from the pointer to `to`, one cell at a time.
    fn glide(&self, to: Position, kind: MouseEventKind, events: &mut VecDeque<Event>) {
        line(self.pointer.unwrap_or(to), to, kind, events);
    }
}

fn mouse(kind: MouseEventKind, at: Position) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: at.x,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    })
}

/// Events of `kind` at each cell after `from` up to `to`, or one at `to` when
/// the two are the same cell.
fn line(from: Position, to: Position, kind: MouseEventKind, events: &mut VecDeque<Event>) {
    let (dx, dy) = (
        i32::from(to.x) - i32::from(from.x),
        i32::from(to.y) - i32::from(from.y),
    );
    let cells = dx.abs().max(dy.abs()).max(1);
    for step in 1..=cells {
        let x = i32::from(from.x) + dx * step / cells;
        let y = i32::from(from.y) + dy * step / cells;
        events.push_back(mouse(kind, Position::new(x as u16, y as u16)));
    }
}

/// Where `target` is on `screen`.
pub fn find(screen: &Buffer, target: &Target) -> Option<Position> {
    let (text, nth, anchor) = match target {
        Target::Text(text) => (text.as_str(), 0, Anchor::Middle),
        Target::Match { text, nth, at } => (text.as_str(), *nth, *at),
    };
    let area = screen.area;
    let matches: Vec<Position> = (area.y..area.bottom())
        .flat_map(|y| {
            let mut row = String::new();
            let mut columns = Vec::new();
            let mut x = area.x;
            while x < area.right() {
                let symbol = screen[(x, y)].symbol();
                columns.push((row.len(), x));
                row.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            let column = |byte: usize| {
                columns
                    .iter()
                    .rev()
                    .find(|(at, _)| *at <= byte)
                    .map_or(area.x, |(_, x)| *x)
            };
            row.match_indices(text)
                .map(|(byte, found)| {
                    let start = column(byte);
                    let end = column(byte + found.len() - 1);
                    let x = match anchor {
                        Anchor::Start => start,
                        Anchor::Middle => (start + end) / 2,
                        Anchor::End => end,
                    };
                    Position::new(x, y)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let index = if nth < 0 {
        matches.len().checked_sub(nth.unsigned_abs() as usize)?
    } else {
        nth as usize
    };
    matches.get(index).copied()
}

/// The key named `name`: a named key such as `Enter`, or a single character,
/// after any `Alt+`, `Ctrl+` and `Shift+`.
pub fn key_event(name: &str) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    let mut name = name;
    loop {
        let (prefix, modifier) = if let Some(rest) = name.strip_prefix("Alt+") {
            (rest, KeyModifiers::ALT)
        } else if let Some(rest) = name.strip_prefix("Ctrl+") {
            (rest, KeyModifiers::CONTROL)
        } else if let Some(rest) = name.strip_prefix("Shift+") {
            (rest, KeyModifiers::SHIFT)
        } else {
            break;
        };
        if prefix.is_empty() {
            break;
        }
        modifiers |= modifier;
        name = prefix;
    }
    let code = match name {
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        _ => {
            let mut chars = name.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(ch)
        }
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn a_key_name_can_hold_modifiers_before_the_key() {
        let key = key_event("Alt+Enter").unwrap();
        assert_eq!(
            (key.code, key.modifiers),
            (KeyCode::Enter, KeyModifiers::ALT)
        );
        let key = key_event("Ctrl+Shift+a").unwrap();
        assert_eq!(key.code, KeyCode::Char('a'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key_event("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(key_event("Alt+").map(|key| key.code), None);
    }
    use crate::scenario::Drag;

    fn typed(text: &str) -> InputStep {
        InputStep::Type {
            r#type: text.to_string(),
            per_char_ms: Some(10),
        }
    }

    fn key(name: &str) -> InputStep {
        InputStep::Key {
            key: name.to_string(),
        }
    }

    fn screen(rows: &[&str]) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, rows.len() as u16));
        for (y, row) in rows.iter().enumerate() {
            buffer.set_string(0, y as u16, row, ratatui::style::Style::new());
        }
        buffer
    }

    fn code(event: Option<Event>) -> Option<KeyCode> {
        match event {
            Some(Event::Key(key)) => Some(key.code),
            _ => None,
        }
    }

    fn placed(text: &str, nth: i64, at: Anchor) -> Target {
        Target::Match {
            text: text.to_string(),
            nth,
            at,
        }
    }

    #[test]
    fn typing_presses_one_character_per_interval() {
        let blank = screen(&[""]);
        let mut player = Player::new(vec![typed("ab")]).unwrap();
        let start = Instant::now();
        assert_eq!(
            code(player.next(start, true, &blank)),
            Some(KeyCode::Char('a'))
        );
        assert_eq!(player.next(start, true, &blank), None);
        let later = start + Duration::from_millis(10);
        assert_eq!(
            code(player.next(later, true, &blank)),
            Some(KeyCode::Char('b'))
        );
        assert_eq!(
            player.next(later + Duration::from_secs(1), true, &blank),
            None
        );
        assert_eq!(player.due(), None);
    }

    #[test]
    fn a_wait_for_idle_holds_the_next_step_until_the_turn_ends() {
        let blank = screen(&[""]);
        let steps = vec![key("Enter"), InputStep::Wait { wait: Wait::Idle }, key("j")];
        let mut player = Player::new(steps).unwrap();
        let mut now = Instant::now();
        assert_eq!(code(player.next(now, true, &blank)), Some(KeyCode::Enter));
        for _ in 0..5 {
            now += Duration::from_secs(1);
            assert_eq!(player.next(now, false, &blank), None);
        }
        now += Duration::from_secs(1);
        assert_eq!(
            code(player.next(now, true, &blank)),
            Some(KeyCode::Char('j'))
        );
    }

    #[test]
    fn a_key_name_that_is_neither_a_named_key_nor_one_character_is_refused() {
        assert!(Player::new(vec![key("Enterr")]).is_err());
        assert!(Player::new(vec![key("PageDown"), key("j")]).is_ok());
    }

    #[test]
    fn a_target_is_found_by_its_text_its_match_from_either_end_and_the_end_it_names() {
        let shown = screen(&["one blah two", "  blah ① mark"]);
        let text = |text: &str| Target::Text(text.to_string());
        assert_eq!(find(&shown, &text("two")), Some(Position::new(10, 0)));
        assert_eq!(
            find(&shown, &placed("blah", 1, Anchor::Start)),
            Some(Position::new(2, 1))
        );
        assert_eq!(
            find(&shown, &placed("blah", 0, Anchor::End)),
            Some(Position::new(7, 0))
        );
        assert_eq!(find(&shown, &text("mark")), Some(Position::new(10, 1)));
        assert_eq!(find(&shown, &placed("blah", 2, Anchor::Start)), None);
        assert_eq!(
            find(&shown, &placed("blah", -1, Anchor::Start)),
            Some(Position::new(2, 1))
        );
        assert_eq!(
            find(&shown, &placed("blah", -2, Anchor::Start)),
            Some(Position::new(4, 0))
        );
        assert_eq!(find(&shown, &placed("blah", -3, Anchor::Start)), None);
        assert_eq!(find(&shown, &text("absent")), None);
    }

    #[test]
    fn a_drag_glides_cell_by_cell_at_an_even_pace_and_ends_on_its_target() {
        let shown = screen(&["from here", "", "", "to there"]);
        let drag = InputStep::Drag {
            drag: Drag {
                from: placed("from", 0, Anchor::Start),
                to: placed("there", 0, Anchor::End),
            },
            ms: Some(100),
        };
        let mut player = Player::new(vec![drag]).unwrap();
        let start = Instant::now();
        let mut played = Vec::new();
        let mut now = start;
        while player.due().is_some() {
            now = player.due().unwrap().max(now);
            if let Some(Event::Mouse(mouse)) = player.next(now, true, &shown) {
                played.push((now - start, mouse.kind, mouse.column, mouse.row));
            }
        }
        let kinds: Vec<_> = played.iter().map(|(_, kind, _, _)| *kind).collect();
        let left = MouseButton::Left;
        assert_eq!(kinds[0], MouseEventKind::Moved);
        assert_eq!(kinds[1], MouseEventKind::Down(left));
        assert!(
            kinds[2..kinds.len() - 1]
                .iter()
                .all(|kind| *kind == MouseEventKind::Drag(left))
        );
        assert_eq!(
            played.last().map(|p| (p.1, p.2, p.3)),
            Some((MouseEventKind::Up(left), 7, 3))
        );
        let gaps: Vec<_> = played
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .collect();
        assert!(gaps.iter().all(|gap| *gap == gaps[0]), "{gaps:?}");
        let columns: Vec<_> = played[1..].iter().map(|(_, _, x, _)| *x).collect();
        assert!(
            columns.windows(2).all(|pair| pair[1] >= pair[0]),
            "{columns:?}"
        );
    }

    #[test]
    fn a_click_waits_for_its_text_and_stops_the_playback_when_it_never_comes() {
        let blank = screen(&[""]);
        let click = InputStep::Click {
            click: Target::Text("mark".to_string()),
            ms: None,
        };
        let mut player = Player::new(vec![click, key("j")]).unwrap();
        let start = Instant::now();
        assert_eq!(player.next(start, true, &blank), None);
        assert!(player.due().is_some());
        let shown = screen(&["mark"]);
        let first = player.next(start + RECHECK, true, &shown);
        assert!(matches!(first, Some(Event::Mouse(_))), "{first:?}");

        let mut lost = Player::new(vec![
            InputStep::Click {
                click: Target::Text("mark".to_string()),
                ms: None,
            },
            key("j"),
        ])
        .unwrap();
        assert_eq!(lost.next(start, true, &blank), None);
        assert_eq!(lost.next(start + LOST_FOR + RECHECK, true, &blank), None);
        assert_eq!(lost.due(), None);
    }
}
