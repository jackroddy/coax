use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::SessionId;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, timeout_at};

use crate::agents::Listing;
use crate::app::Keys;
use crate::config::Config;
use crate::header;
use crate::palette;
use crate::ui;

/// How long the picker waits for the agents' session lists before its first
/// draw. A slower agent's sessions appear when they arrive.
const LISTING_WAIT: Duration = Duration::from_millis(300);

/// A session older than this shows its date in place of its age.
const RECENT_DAYS: i64 = 7;

/// The agent picker shown on a bare launch.
pub struct Picker<'a> {
    names: Vec<&'a str>,
    commands: Vec<String>,
    default: Option<usize>,
    sessions: Vec<SessionRow>,
    selected: Selected,

    /// Whether the user has moved the highlight; until then it follows the
    /// newest session as listings arrive.
    moved: bool,
    path: &'a Path,
}

struct SessionRow {
    /// The agent's index in `Picker::names`.
    agent: usize,
    id: SessionId,
    title: String,
    updated: Option<Updated>,
}

struct Updated {
    seconds: i64,
    date: String,
}

/// The highlighted row. A session is kept by identity, so sessions arriving
/// above it do not move the highlight.
#[derive(PartialEq, Eq)]
enum Selected {
    Session(usize, SessionId),
    Agent(usize),
}

/// What a key press in the picker asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    Agent(&'a str),
    Session { agent: &'a str, id: SessionId },
    Quit,
}

impl<'a> Picker<'a> {
    pub fn new(config: &'a Config, path: &'a Path) -> Picker<'a> {
        let names: Vec<&str> = config.agents.keys().map(String::as_str).collect();
        let commands = config
            .agents
            .values()
            .map(|agent| {
                let mut words = vec![home_as_tilde(&agent.command().display().to_string())];
                words.extend(agent.arguments().iter().cloned());
                words.join(" ")
            })
            .collect();
        let default = config
            .default
            .as_deref()
            .and_then(|default| names.iter().position(|name| *name == default));
        Picker {
            names,
            commands,
            default,
            sessions: Vec::new(),
            selected: Selected::Agent(default.unwrap_or(0)),
            moved: false,
            path,
        }
    }

    /// Replace the sessions of the listed agent. Rows stay newest first.
    pub fn listing(&mut self, listing: Listing) {
        let Some(agent) = self.names.iter().position(|name| *name == listing.agent) else {
            return;
        };
        self.sessions.retain(|row| row.agent != agent);
        self.sessions
            .extend(listing.sessions.into_iter().map(|info| SessionRow {
                agent,
                title: info.title.unwrap_or_else(|| "untitled".to_string()),
                updated: info.updated_at.and_then(|date| {
                    Some(Updated {
                        seconds: unix_seconds(&date)?,
                        date,
                    })
                }),
                id: info.session_id,
            }));
        self.sessions
            .sort_by_key(|row| std::cmp::Reverse(row.updated.as_ref().map(|at| at.seconds)));
        if !self.moved {
            self.selected = match self.sessions.first() {
                Some(row) => Selected::Session(row.agent, row.id.clone()),
                None => Selected::Agent(self.default.unwrap_or(0)),
            };
            return;
        }
        if let Selected::Session(agent, id) = &self.selected {
            let kept = self
                .sessions
                .iter()
                .any(|row| row.agent == *agent && row.id == *id);
            if !kept {
                self.selected = Selected::Agent(self.default.unwrap_or(0));
            }
        }
    }

    fn rows(&self) -> usize {
        self.sessions.len() + self.names.len()
    }

    /// The highlighted row's place in the sessions-then-agents list.
    fn position(&self) -> usize {
        match &self.selected {
            Selected::Session(agent, id) => self
                .sessions
                .iter()
                .position(|row| row.agent == *agent && row.id == *id)
                .unwrap_or(0),
            Selected::Agent(index) => self.sessions.len() + index,
        }
    }

    fn select(&mut self, position: usize) {
        self.moved = true;
        self.selected = match self.sessions.get(position) {
            Some(row) => Selected::Session(row.agent, row.id.clone()),
            None => Selected::Agent(position - self.sessions.len()),
        };
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<Choice<'a>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Some(Choice::Quit),
            KeyCode::Esc => Some(Choice::Quit),
            KeyCode::Up | KeyCode::Char('k') => {
                self.select(self.position().saturating_sub(1));
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.names.is_empty() {
                    return None;
                }
                self.select((self.position() + 1).min(self.rows() - 1));
                None
            }
            KeyCode::Enter => match &self.selected {
                Selected::Session(agent, id) => Some(Choice::Session {
                    agent: self.names[*agent],
                    id: id.clone(),
                }),
                Selected::Agent(index) => self.names.get(*index).map(|name| Choice::Agent(name)),
            },
            _ => None,
        }
    }

    fn draw(&self, frame: &mut Frame, now: i64) {
        let agents_height = self.names.len().max(2) as u16 + 2;
        let sessions = !self.sessions.is_empty();
        let room = frame.area().height.saturating_sub(3 + agents_height);
        let sessions_height = (self.sessions.len() as u16 + 2).min(room);
        let [header_area, sessions_area, agents_area, footer_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(if sessions { sessions_height } else { 0 }),
            Constraint::Min(agents_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let title = if sessions {
            "pick a session or an agent"
        } else {
            "pick an agent"
        };
        header::title_bar(frame, header_area, title);
        let on_session = matches!(self.selected, Selected::Session(..));
        if sessions {
            self.draw_sessions(frame, sessions_area, on_session, now);
        }
        self.draw_agents(frame, agents_area, !on_session);
        let hints = if self.names.is_empty() {
            "esc quit"
        } else {
            "j/k move   enter start   esc quit"
        };
        frame.render_widget(Paragraph::new(hints), footer_area);
    }

    fn draw_sessions(&self, frame: &mut Frame, area: Rect, focused: bool, now: i64) {
        let block = ui::pane("sessions", focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = inner.height as usize;
        let position = self.position();
        let first = if position < self.sessions.len() {
            (position + 1).saturating_sub(height)
        } else {
            0
        };
        let lines: Vec<Line> = (first..self.sessions.len().min(first + height))
            .map(|index| self.session_row(index, inner.width as usize, now))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn session_row(&self, index: usize, width: usize, now: i64) -> Line<'static> {
        let row = &self.sessions[index];
        let chosen = self.position() == index;
        let gutter = if chosen { "› " } else { "  " };
        let agent_width = self
            .names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0);
        let ages: Vec<String> = self
            .sessions
            .iter()
            .map(|row| {
                row.updated
                    .as_ref()
                    .map(|at| age(at, now))
                    .unwrap_or_default()
            })
            .collect();
        let age_width = ages
            .iter()
            .map(|age| age.chars().count())
            .max()
            .unwrap_or(0);
        let title_width = width.saturating_sub(2 + 2 + agent_width + 2 + age_width);
        let title = ui::cut(&row.title, title_width);
        let line = Line::from(vec![
            Span::raw(gutter),
            Span::styled(
                format!("{title:<title_width$}  "),
                Style::new().fg(palette::get().cyan),
            ),
            Span::styled(
                format!("{:<agent_width$}  ", self.names[row.agent]),
                ui::dim(),
            ),
            Span::styled(format!("{:>age_width$}", ages[index]), ui::dim()),
        ]);
        if chosen {
            line.style(Style::new().bg(palette::get().selection))
        } else {
            line
        }
    }

    fn draw_agents(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = ui::pane("agents", focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = if self.names.is_empty() {
            vec![
                Line::styled("no agents configured", ui::dim()),
                Line::styled(
                    format!(
                        "add one to {}",
                        home_as_tilde(&self.path.display().to_string())
                    ),
                    ui::dim(),
                ),
            ]
        } else {
            (0..self.names.len())
                .map(|index| self.row(index, inner.width as usize))
                .collect()
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn row(&self, index: usize, width: usize) -> Line<'static> {
        let name_width = self
            .names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0);
        let chosen = self.selected == Selected::Agent(index);
        let gutter = if chosen { "› " } else { "  " };
        let tail = if self.default == Some(index) {
            "  default"
        } else {
            ""
        };
        let room = width.saturating_sub(2 + name_width + 2 + tail.chars().count());
        let command = ui::cut(&self.commands[index], room);
        let used = 2 + name_width + 2 + command.chars().count() + tail.chars().count();
        let line = Line::from(vec![
            Span::raw(gutter),
            Span::styled(
                format!("{:<name_width$}  ", self.names[index]),
                Style::new().fg(palette::get().cyan),
            ),
            Span::styled(command, ui::dim()),
            Span::raw(" ".repeat(width.saturating_sub(used))),
            Span::styled(tail, ui::dim()),
        ]);
        if chosen {
            line.style(Style::new().bg(palette::get().selection))
        } else {
            line
        }
    }
}

fn home_as_tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// `at` as an age against `now`: `just now`, `3m ago`, `5h ago`, `2d ago`, and
/// past a week the date.
fn age(at: &Updated, now: i64) -> String {
    let seconds = now - at.seconds;
    match seconds {
        ..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ if seconds < RECENT_DAYS * 86_400 => format!("{}d ago", seconds / 86_400),
        _ => at.date.chars().take(10).collect(),
    }
}

/// Seconds since the Unix epoch of an ISO 8601 timestamp such as
/// `2026-11-03T10:22:05Z`, `2026-11-03T10:22:05.123+01:00`.
fn unix_seconds(iso: &str) -> Option<i64> {
    let (date, time) = iso.split_once(['T', ' '])?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let clock_end = time.find(['Z', '+', '-']).unwrap_or(time.len());
    let (clock, zone) = time.split_at(clock_end);
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock
        .next()
        .unwrap_or("0")
        .split('.')
        .next()?
        .parse()
        .ok()?;
    let offset = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (hours, minutes) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let minutes_total = hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?;
            if sign == '+' {
                minutes_total * 60
            } else {
                -minutes_total * 60
            }
        }
        _ => 0,
    };

    // days since 1970-01-01 of a proleptic Gregorian date
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// Show the picker until a row is chosen or the user quits. The agents'
/// session lists arrive on `listings`.
pub async fn pick<'a>(
    config: &'a Config,
    path: &'a Path,
    listings: &mut UnboundedReceiver<Listing>,
    keys: &mut Keys,
) -> io::Result<Choice<'a>> {
    let mut picker = Picker::new(config, path);
    let mut reported = HashSet::new();
    let deadline = Instant::now() + LISTING_WAIT;
    while reported.len() < config.agents.len() {
        match timeout_at(deadline, listings.recv()).await {
            Ok(Some(listing)) => {
                reported.insert(listing.agent.clone());
                picker.listing(listing);
            }
            _ => break,
        }
    }
    let mut terminal = ratatui::init();
    let mut listing_open = true;
    let result = loop {
        if let Err(error) = terminal.draw(|frame| picker.draw(frame, now())) {
            break Err(error);
        }
        tokio::select! {
            key = keys.recv() => match key {
                Some(Ok(Event::Key(key))) => {
                    if let Some(choice) = picker.key(key) {
                        break Ok(choice);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => break Err(error),
                None => break Ok(Choice::Quit),
            },
            listing = listings.recv(), if listing_open => match listing {
                Some(listing) => picker.listing(listing),
                None => listing_open = false,
            },
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::SessionInfo;

    use super::*;

    fn config(default: Option<&str>) -> Config {
        let mut config: Config = toml::from_str(
            r#"
            [agents.alpha]
            command = "a"
            [agents.beta]
            command = "b"
            [agents.gamma]
            command = "c"
            "#,
        )
        .unwrap();
        config.default = default.map(str::to_string);
        config
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_with_no_default_starts_the_first_agent_by_name() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("alpha"))
        );
    }

    #[test]
    fn the_default_agent_starts_highlighted() {
        let config = config(Some("beta"));
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("beta"))
        );
    }

    #[test]
    fn moving_past_either_end_of_the_list_stays_on_the_end() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.key(press(KeyCode::Up));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("alpha"))
        );
        for _ in 0..5 {
            picker.key(press(KeyCode::Char('j')));
        }
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("gamma"))
        );
    }

    #[test]
    fn enter_on_an_empty_list_does_nothing_and_esc_quits() {
        let config = Config::default();
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        assert_eq!(picker.key(press(KeyCode::Enter)), None);
        assert_eq!(picker.key(press(KeyCode::Esc)), Some(Choice::Quit));
    }

    fn listing_of(agent: &str, sessions: &[(&str, &str, &str)]) -> Listing {
        Listing {
            agent: agent.to_string(),
            sessions: sessions
                .iter()
                .map(|(id, title, updated)| {
                    SessionInfo::new(id.to_string(), "/work")
                        .title(title.to_string())
                        .updated_at(updated.to_string())
                })
                .collect(),
        }
    }

    #[test]
    fn sessions_from_every_agent_are_listed_newest_first() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[("a1", "old", "2026-11-01T10:00:00Z")],
        ));
        picker.listing(listing_of(
            "gamma",
            &[
                ("g1", "newest", "2026-11-08T10:00:00Z"),
                ("g2", "middle", "2026-11-05T10:00:00Z"),
            ],
        ));
        let titles: Vec<&str> = picker.sessions.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["newest", "middle", "old"]);
    }

    #[test]
    fn the_cursor_starts_on_the_newest_session() {
        let config = config(Some("beta"));
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[
                ("a1", "older", "2026-11-01T10:00:00Z"),
                ("a2", "newer", "2026-11-05T10:00:00Z"),
            ],
        ));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Session {
                agent: "alpha",
                id: SessionId::new("a2".to_string())
            })
        );
    }

    #[test]
    fn moving_down_from_the_newest_session_reaches_the_older_one_then_the_agents() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "beta",
            &[
                ("b1", "newer", "2026-11-08T10:00:00Z"),
                ("b2", "older", "2026-11-02T10:00:00Z"),
            ],
        ));
        picker.key(press(KeyCode::Char('j')));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Session {
                agent: "beta",
                id: SessionId::new("b2".to_string())
            })
        );
        picker.key(press(KeyCode::Char('j')));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("alpha"))
        );
    }

    #[test]
    fn the_highlight_follows_the_newest_session_until_the_user_moves_it() {
        let config = config(Some("gamma"));
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[("a1", "one", "2026-11-01T10:00:00Z")],
        ));
        picker.listing(listing_of("beta", &[("b1", "two", "2026-11-02T10:00:00Z")]));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Session {
                agent: "beta",
                id: SessionId::new("b1".to_string())
            })
        );
        picker.key(press(KeyCode::Char('j')));
        picker.listing(listing_of(
            "gamma",
            &[("c1", "three", "2026-11-03T10:00:00Z")],
        ));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Session {
                agent: "alpha",
                id: SessionId::new("a1".to_string())
            })
        );
    }

    #[test]
    fn an_agent_that_reports_no_sessions_drops_the_ones_it_had() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[("a1", "one", "2026-11-01T10:00:00Z")],
        ));
        picker.listing(listing_of("alpha", &[]));
        assert!(picker.sessions.is_empty());
    }

    fn updated(iso: &str) -> Updated {
        Updated {
            seconds: unix_seconds(iso).unwrap(),
            date: iso.to_string(),
        }
    }

    #[test]
    fn an_iso_timestamp_reads_as_unix_seconds_with_its_offset() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-11-03T10:22:05Z"), Some(1_793_701_325));
        assert_eq!(
            unix_seconds("2026-11-03T10:22:05.250+01:00"),
            Some(1_793_697_725)
        );
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn an_age_reads_as_minutes_hours_days_and_then_the_date() {
        let at = updated("2026-11-03T10:00:00Z");
        let start = at.seconds;
        assert_eq!(age(&at, start + 30), "just now");
        assert_eq!(age(&at, start + 3 * 60), "3m ago");
        assert_eq!(age(&at, start + 5 * 3_600), "5h ago");
        assert_eq!(age(&at, start + 2 * 86_400), "2d ago");
        assert_eq!(age(&at, start + 9 * 86_400), "2026-11-03");
    }
}
