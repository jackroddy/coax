use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::SessionId;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, timeout_at};
use unicode_width::UnicodeWidthStr;

use crate::agents::{Failure, Listing};
use crate::app::Keys;
use crate::config::Config;
use crate::header;
use crate::palette;
use crate::playback::Player;
use crate::ui;

/// How long the picker waits for the agents' session lists before its first
/// draw. A slower agent's sessions appear when they arrive.
const LISTING_WAIT: Duration = Duration::from_millis(300);

/// A session older than this shows its date in place of its age.
const RECENT_DAYS: i64 = 7;

/// The mark before a ready agent.
const READY: char = '●';

/// The mark before an agent that failed to start.
const FAILED: char = '✗';

/// The frames of the mark before an agent still starting.
const STARTING: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The gutter of the highlighted row in the focused pane.
const CHOSEN: &str = "› ";

/// How often the starting mark moves to its next frame.
const SPIN: Duration = Duration::from_millis(100);

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The agent picker shown on a bare launch.
pub struct Picker<'a> {
    names: Vec<&'a str>,
    commands: Vec<String>,

    /// Why each agent has no sessions, when it has none for a reason.
    failures: Vec<Option<Failure>>,

    /// Which agents have sent their listing, so are no longer starting.
    reported: Vec<bool>,
    default: Option<usize>,
    sessions: Vec<SessionRow>,
    selected: Selected,

    /// The agent row to return to when focus comes back to the new pane.
    agent_cursor: usize,

    /// The session row to return to when focus comes back to the sessions.
    session_cursor: Option<(usize, SessionId)>,

    /// Whether the user has moved the highlight; until then it follows the
    /// newest session as listings arrive.
    moved: bool,
    path: &'a Path,
}

struct SessionRow {
    /// The agent's index in `Picker::names`.
    agent: usize,
    id: SessionId,
    title: Option<String>,
    updated: Option<Updated>,
}

struct Updated {
    seconds: i64,
    date: String,
}

/// The highlighted row, which also says which pane has focus. A session is
/// kept by identity, so sessions arriving above it do not move the
/// highlight.
#[derive(PartialEq, Eq)]
enum Selected {
    Session(usize, SessionId),
    Agent(usize),
}

/// What a key press in the picker asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    Agent(&'a str),
    Session {
        agent: &'a str,
        id: SessionId,
        title: Option<String>,
    },
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
            failures: names.iter().map(|_| None).collect(),
            reported: names.iter().map(|_| false).collect(),
            names,
            commands,
            default,
            sessions: Vec::new(),
            selected: Selected::Agent(default.unwrap_or(0)),
            agent_cursor: default.unwrap_or(0),
            session_cursor: None,
            moved: false,
            path,
        }
    }

    /// Replace the sessions of the listed agent. Rows stay newest first.
    pub fn listing(&mut self, listing: Listing) {
        let Some(agent) = self.names.iter().position(|name| *name == listing.agent) else {
            return;
        };
        self.reported[agent] = true;
        self.failures[agent] = listing.failure;
        self.sessions.retain(|row| row.agent != agent);
        self.sessions
            .extend(listing.sessions.into_iter().map(|info| SessionRow {
                agent,
                title: info.title,
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
        if self
            .session_cursor
            .as_ref()
            .is_some_and(|(agent, id)| self.session_index(*agent, id).is_none())
        {
            self.session_cursor = None;
        }
        if !self.moved {
            self.selected = match self.sessions.first() {
                Some(row) => Selected::Session(row.agent, row.id.clone()),
                None => Selected::Agent(self.agent_cursor),
            };
            return;
        }
        if let Selected::Session(agent, id) = &self.selected
            && self.session_index(*agent, id).is_none()
        {
            self.selected = Selected::Agent(self.agent_cursor);
        }
    }

    /// Whether an agent is still starting, so its mark moves.
    pub fn starting(&self) -> bool {
        self.reported.iter().any(|reported| !reported)
    }

    fn session_index(&self, agent: usize, id: &SessionId) -> Option<usize> {
        self.sessions
            .iter()
            .position(|row| row.agent == agent && row.id == *id)
    }

    fn select_session(&mut self, index: usize) {
        let row = &self.sessions[index];
        self.selected = Selected::Session(row.agent, row.id.clone());
    }

    /// The highlighted session's row, or the one focus left behind.
    fn session_row_index(&self) -> Option<usize> {
        match &self.selected {
            Selected::Session(agent, id) => self.session_index(*agent, id),
            Selected::Agent(_) => self
                .session_cursor
                .as_ref()
                .and_then(|(agent, id)| self.session_index(*agent, id)),
        }
    }

    fn focus_new(&mut self) {
        if let Selected::Session(agent, id) = &self.selected {
            self.session_cursor = Some((*agent, id.clone()));
            self.selected = Selected::Agent(self.agent_cursor);
        }
    }

    fn focus_sessions(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        if let Selected::Agent(index) = self.selected {
            self.agent_cursor = index;
            self.select_session(self.session_row_index().unwrap_or(0));
        }
    }

    /// Move the highlight `step` rows within the focused pane, stopping at
    /// either end.
    fn step(&mut self, step: isize) {
        match self.selected {
            Selected::Session(..) => {
                let at = self.session_row_index().unwrap_or(0);
                let to = at.saturating_add_signed(step).min(self.sessions.len() - 1);
                self.select_session(to);
            }
            Selected::Agent(index) if !self.names.is_empty() => {
                let to = index.saturating_add_signed(step).min(self.names.len() - 1);
                self.agent_cursor = to;
                self.selected = Selected::Agent(to);
            }
            Selected::Agent(_) => {}
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<Choice<'a>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Some(Choice::Quit),
            KeyCode::Esc => return Some(Choice::Quit),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Left | KeyCode::Char('h') => self.focus_new(),
            KeyCode::Right | KeyCode::Char('l') => self.focus_sessions(),
            KeyCode::Tab | KeyCode::BackTab => match self.selected {
                Selected::Session(..) => self.focus_new(),
                Selected::Agent(_) => self.focus_sessions(),
            },
            KeyCode::Enter => return self.choice(),
            _ => return None,
        }
        self.moved = true;
        None
    }

    fn choice(&self) -> Option<Choice<'a>> {
        match &self.selected {
            Selected::Session(agent, id) => Some(Choice::Session {
                agent: self.names[*agent],
                id: id.clone(),
                title: self
                    .sessions
                    .iter()
                    .find(|row| row.id == *id)
                    .and_then(|row| row.title.clone()),
            }),
            Selected::Agent(index)
                if matches!(self.failures.get(*index), Some(Some(Failure::Start(_)))) =>
            {
                None
            }
            Selected::Agent(index) => self.names.get(*index).map(|name| Choice::Agent(name)),
        }
    }

    fn draw(&self, frame: &mut Frame, now: i64, spin: usize) {
        let area = frame.area();
        frame.buffer_mut().set_style(area, palette::base());
        let [header_area, body, detail_area, footer_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        header::title_bar(frame, header_area, "pick a session or start a new one");
        let new_width = (self.new_width() as u16).min(body.width / 2);
        let [new_area, sessions_area] =
            Layout::horizontal([Constraint::Length(new_width), Constraint::Min(0)]).areas(body);
        let on_session = matches!(self.selected, Selected::Session(..));
        self.draw_new(frame, new_area, !on_session, spin);
        self.draw_sessions(frame, sessions_area, on_session, now);
        frame.render_widget(
            Paragraph::new(self.detail(detail_area.width as usize)),
            detail_area,
        );
        let hints = if self.names.is_empty() {
            " esc quit"
        } else if self.sessions.is_empty() {
            " j/k move   enter start   esc quit"
        } else {
            " j/k move   h/l or tab switch   enter start   esc quit"
        };
        frame.render_widget(Paragraph::new(Line::styled(hints, ui::dim())), footer_area);
    }

    /// The new pane's width: its widest row, its borders and its padding.
    fn new_width(&self) -> usize {
        let content = if self.names.is_empty() {
            self.no_agents()
                .iter()
                .map(|line| line.width())
                .max()
                .unwrap_or(0)
        } else {
            CHOSEN.width()
                + 2
                + self
                    .names
                    .iter()
                    .map(|name| name.width())
                    .max()
                    .unwrap_or(0)
        };
        content.max(" new ".width()) + 4
    }

    fn no_agents(&self) -> [String; 2] {
        [
            "no agents configured".to_string(),
            format!(
                "add one to {}",
                home_as_tilde(&self.path.display().to_string())
            ),
        ]
    }

    fn draw_new(&self, frame: &mut Frame, area: Rect, focused: bool, spin: usize) {
        let block = pane("new", focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines: Vec<Line> = if self.names.is_empty() {
            self.no_agents()
                .into_iter()
                .map(|line| Line::styled(line, ui::dim()))
                .collect()
        } else {
            (0..self.names.len())
                .map(|index| self.agent_row(index, inner.width as usize, focused, spin))
                .collect()
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn agent_row(&self, index: usize, width: usize, focused: bool, spin: usize) -> Line<'static> {
        let palette = palette::get();
        let chosen = focused && self.selected == Selected::Agent(index);
        let gutter = if chosen { CHOSEN } else { "  " };
        let failed = matches!(self.failures[index], Some(Failure::Start(_)));
        let (mark, mark_style) = if failed {
            (FAILED, Style::new().fg(palette.red))
        } else if !self.reported[index] {
            (
                STARTING[spin % STARTING.len()],
                Style::new().fg(palette.yellow),
            )
        } else {
            (READY, Style::new().fg(palette.green))
        };
        let name_style = if failed {
            ui::dim()
        } else {
            Style::new().fg(palette.cyan)
        };
        let name = ui::cut(self.names[index], width.saturating_sub(gutter.width() + 2));
        let line = Line::from(vec![
            Span::raw(gutter),
            Span::styled(format!("{mark} "), mark_style),
            Span::styled(name, name_style),
        ]);
        if chosen {
            line.style(Style::new().bg(palette.selection))
        } else {
            line
        }
    }

    fn draw_sessions(&self, frame: &mut Frame, area: Rect, focused: bool, now: i64) {
        let block = pane("sessions", focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.sessions.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("no earlier sessions here", ui::dim())),
                inner,
            );
            return;
        }
        let height = inner.height as usize;
        let at = self.session_row_index().unwrap_or(0);
        let first = (at + 1).saturating_sub(height);
        let chosen = focused.then_some(at);
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
        let lines: Vec<Line> = (first..self.sessions.len().min(first + height))
            .map(|index| self.session_row(index, inner.width as usize, chosen, &ages))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn session_row(
        &self,
        index: usize,
        width: usize,
        chosen: Option<usize>,
        ages: &[String],
    ) -> Line<'static> {
        let row = &self.sessions[index];
        let chosen = chosen == Some(index);
        let gutter = if chosen { CHOSEN } else { "  " };
        let agent_width = self
            .names
            .iter()
            .map(|name| name.width())
            .max()
            .unwrap_or(0);
        let age_width = ages.iter().map(|age| age.width()).max().unwrap_or(0);
        let title_width = width.saturating_sub(gutter.width() + 2 + agent_width + 2 + age_width);
        let title = ui::cut(row.title.as_deref().unwrap_or("untitled"), title_width);
        let line = Line::from(vec![
            Span::raw(gutter),
            Span::styled(
                format!("{}  ", ui::pad_right(&title, title_width)),
                Style::new().fg(palette::get().cyan),
            ),
            Span::styled(
                format!("{}  ", ui::pad_left(self.names[row.agent], agent_width)),
                ui::dim(),
            ),
            Span::styled(ui::pad_left(&ages[index], age_width), ui::dim()),
        ]);
        if chosen {
            line.style(Style::new().bg(palette::get().selection))
        } else {
            line
        }
    }

    /// The line under the panes: the highlighted agent's command, or why it
    /// failed, and on the right its status and its number of sessions.
    fn detail(&self, width: usize) -> Line<'static> {
        let agent = match self.selected {
            Selected::Session(agent, _) => agent,
            Selected::Agent(index) => index,
        };
        if agent >= self.names.len() {
            return Line::default();
        }
        let failure = &self.failures[agent];
        let (left, left_style) = match failure {
            Some(Failure::Start(error)) => (
                format!("failed: {}", first_line(error)),
                Style::new().fg(palette::get().red),
            ),
            Some(Failure::List(error)) => {
                (format!("sessions failed: {}", first_line(error)), ui::dim())
            }
            None => (self.commands[agent].clone(), ui::dim()),
        };
        let mut right = vec![
            match failure {
                Some(Failure::Start(_)) => "failed",
                _ if !self.reported[agent] => "starting",
                _ => "ready",
            }
            .to_string(),
        ];
        if self.default == Some(agent) {
            right.push("default".to_string());
        }
        if self.reported[agent] && !matches!(failure, Some(Failure::Start(_))) {
            let count = self
                .sessions
                .iter()
                .filter(|row| row.agent == agent)
                .count();
            right.push(match count {
                1 => "1 session".to_string(),
                count => format!("{count} sessions"),
            });
        }
        let right = format!("{} ", right.join(" · "));
        let room = width.saturating_sub(1 + right.width() + 2);
        let left = ui::cut(&left, room);
        let gap = width.saturating_sub(1 + left.width() + right.width());
        Line::from(vec![
            Span::raw(" "),
            Span::styled(left, left_style),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, ui::dim()),
        ])
    }
}

/// A picker pane: a rounded border with `title`, in the accent colour while
/// it has focus and dim otherwise.
fn pane(title: &str, focused: bool) -> Block<'static> {
    let (border, title_style) = if focused {
        let accent = Style::new().fg(palette::get().accent);
        (accent, accent.add_modifier(Modifier::BOLD))
    } else {
        (ui::dim(), ui::dim())
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(Padding::horizontal(1))
        .border_style(border)
        .title(Span::styled(format!(" {title} "), title_style))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
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

/// `at` as a short age against `now`: `now`, `3m`, `5h`, `2d`, then past a
/// week the date as `nov 18`, with the year once it is past a year.
fn age(at: &Updated, now: i64) -> String {
    let seconds = now - at.seconds;
    match seconds {
        ..60 => "now".to_string(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ if seconds < RECENT_DAYS * 86_400 => format!("{}d", seconds / 86_400),
        _ => {
            let mut parts = at.date.get(..10).unwrap_or_default().split('-');
            let year = parts.next().unwrap_or_default();
            let month: usize = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
            let day: u32 = parts.next().and_then(|d| d.parse().ok()).unwrap_or(0);
            let Some(month) = MONTHS.get(month.wrapping_sub(1)) else {
                return at.date.chars().take(10).collect();
            };
            if seconds < 365 * 86_400 {
                format!("{month} {day}")
            } else {
                format!("{month} {day} {year}")
            }
        }
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
    player: &mut Player,
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
    let mut screen = ratatui::buffer::Buffer::default();
    let mut spin = 0;
    let mut tick = tokio::time::interval(SPIN);
    let result = loop {
        match terminal.draw(|frame| picker.draw(frame, now(), spin)) {
            Ok(frame) if player.due().is_some() => screen = frame.buffer.clone(),
            Ok(_) => {}
            Err(error) => break Err(error),
        }
        let played = player.due();
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
            () = async { tokio::time::sleep_until(played.unwrap().into()).await }, if played.is_some() => {
                if let Some(Event::Key(key)) = player.next(std::time::Instant::now(), true, &screen)
                    && let Some(choice) = picker.key(key)
                {
                    break Ok(choice);
                }
            },
            _ = tick.tick(), if picker.starting() => spin += 1,
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
            failure: None,
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
        let titles: Vec<&str> = picker
            .sessions
            .iter()
            .map(|r| r.title.as_deref().unwrap())
            .collect();
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
                id: SessionId::new("a2".to_string()),
                title: Some("newer".to_string()),
            })
        );
    }

    #[test]
    fn moving_down_stays_within_the_sessions_and_h_and_l_cross_between_the_panes() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "beta",
            &[
                ("b1", "newer", "2026-11-08T10:00:00Z"),
                ("b2", "older", "2026-11-02T10:00:00Z"),
            ],
        ));
        let older = Some(Choice::Session {
            agent: "beta",
            id: SessionId::new("b2".to_string()),
            title: Some("older".to_string()),
        });
        picker.key(press(KeyCode::Char('j')));
        assert_eq!(picker.key(press(KeyCode::Enter)), older);
        picker.key(press(KeyCode::Char('j')));
        assert_eq!(picker.key(press(KeyCode::Enter)), older);
        picker.key(press(KeyCode::Char('h')));
        picker.key(press(KeyCode::Char('j')));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("beta"))
        );
        picker.key(press(KeyCode::Char('l')));
        assert_eq!(picker.key(press(KeyCode::Enter)), older);
        picker.key(press(KeyCode::Tab));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("beta"))
        );
        picker.key(press(KeyCode::Right));
        picker.key(press(KeyCode::Left));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("beta"))
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
                id: SessionId::new("b1".to_string()),
                title: Some("two".to_string()),
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
                id: SessionId::new("a1".to_string()),
                title: Some("one".to_string()),
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
    fn an_age_reads_short_as_minutes_hours_days_then_the_month_and_day() {
        let at = updated("2026-11-03T10:00:00Z");
        let start = at.seconds;
        assert_eq!(age(&at, start + 30), "now");
        assert_eq!(age(&at, start + 3 * 60), "3m");
        assert_eq!(age(&at, start + 5 * 3_600), "5h");
        assert_eq!(age(&at, start + 2 * 86_400), "2d");
        assert_eq!(age(&at, start + 9 * 86_400), "nov 3");
        assert_eq!(age(&at, start + 400 * 86_400), "nov 3 2026");
    }

    fn failed(agent: &str, failure: Failure) -> Listing {
        Listing {
            failure: Some(failure),
            ..listing_of(agent, &[])
        }
    }

    fn drawn(picker: &Picker) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| picker.draw(frame, 0, 0)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen(picker: &Picker) -> String {
        let buffer = drawn(picker);
        (0..20)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_agent_that_failed_to_start_shows_why_and_cannot_be_chosen() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(failed(
            "alpha",
            Failure::Start("a: No such file".to_string()),
        ));
        assert!(screen(&picker).contains("failed: a: No such file"));
        assert_eq!(picker.key(press(KeyCode::Enter)), None);
        picker.key(press(KeyCode::Down));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("beta"))
        );
    }

    #[test]
    fn an_agent_that_failed_to_list_still_starts() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(failed("alpha", Failure::List("bad cursor".to_string())));
        assert!(screen(&picker).contains("sessions failed: bad cursor"));
        assert_eq!(
            picker.key(press(KeyCode::Enter)),
            Some(Choice::Agent("alpha"))
        );
    }

    fn find(screen: &str, text: &str) -> (usize, usize) {
        screen
            .lines()
            .enumerate()
            .find_map(|(y, row)| row.find(text).map(|byte| (row[..byte].chars().count(), y)))
            .unwrap_or_else(|| panic!("{text} not on\n{screen}"))
    }

    #[test]
    fn the_new_pane_sits_left_of_the_sessions_pane_on_the_same_rows() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[("a1", "one", "2026-11-01T10:00:00Z")],
        ));
        let screen = screen(&picker);
        let (new_x, new_y) = find(&screen, "╭ new ");
        let (sessions_x, sessions_y) = find(&screen, "╭ sessions ");
        assert_eq!(new_y, sessions_y);
        assert!(new_x < sessions_x, "{screen}");
        let (alpha_x, alpha_y) = find(&screen, "alpha");
        let (one_x, one_y) = find(&screen, "› one");
        assert_eq!(alpha_y, one_y);
        assert!(alpha_x < one_x);
    }

    #[test]
    fn only_the_focused_pane_has_the_accent_border_and_the_highlight() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[("a1", "one", "2026-11-01T10:00:00Z")],
        ));
        let accent = palette::get().accent;
        let title_fg = |picker: &Picker, title: &str| {
            let (x, y) = find(&screen(picker), title);
            drawn(picker)[(x as u16 + 2, y as u16)].fg
        };
        assert_eq!(title_fg(&picker, "╭ sessions "), accent);
        assert_ne!(title_fg(&picker, "╭ new "), accent);
        assert!(screen(&picker).contains("› one"));
        assert!(!screen(&picker).contains("› ●"));
        picker.key(press(KeyCode::Tab));
        assert_eq!(title_fg(&picker, "╭ new "), accent);
        assert_ne!(title_fg(&picker, "╭ sessions "), accent);
        assert!(screen(&picker).contains("› ●"));
        assert!(!screen(&picker).contains("› one"));
    }

    #[test]
    fn an_agent_is_marked_starting_then_ready_or_failed() {
        let config = config(None);
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        let mark = |picker: &Picker, name: &str| {
            let screen = screen(picker);
            let (x, y) = find(&screen, name);
            screen.lines().nth(y).unwrap().chars().nth(x - 2).unwrap()
        };
        assert_eq!(mark(&picker, "alpha"), STARTING[0]);
        assert!(picker.starting());
        picker.listing(listing_of("alpha", &[]));
        picker.listing(failed("beta", Failure::Start("b: gone".to_string())));
        picker.listing(listing_of("gamma", &[]));
        assert_eq!(mark(&picker, "alpha"), READY);
        assert_eq!(mark(&picker, "beta"), FAILED);
        assert!(!picker.starting());
    }

    #[test]
    fn the_line_under_the_panes_shows_the_agents_command_status_and_sessions() {
        let config = config(Some("alpha"));
        let mut picker = Picker::new(&config, Path::new("config.toml"));
        picker.listing(listing_of(
            "alpha",
            &[
                ("a1", "one", "2026-11-01T10:00:00Z"),
                ("a2", "two", "2026-11-02T10:00:00Z"),
            ],
        ));
        let screen = screen(&picker);
        let detail = screen.lines().nth(18).unwrap();
        assert!(detail.starts_with(" a "), "{detail}");
        assert!(
            detail.trim_end().ends_with("ready · default · 2 sessions"),
            "{detail}"
        );
        picker.key(press(KeyCode::Char('h')));
        picker.key(press(KeyCode::Char('j')));
        let screen = self::screen(&picker);
        let detail = screen.lines().nth(18).unwrap();
        assert!(detail.starts_with(" b "), "{detail}");
        assert!(detail.trim_end().ends_with("starting"), "{detail}");
    }

    #[test]
    fn an_empty_folder_and_an_empty_config_each_say_so_in_their_pane() {
        let config = config(None);
        let picker = Picker::new(&config, Path::new("config.toml"));
        assert!(screen(&picker).contains("no earlier sessions here"));
        let empty = Config::default();
        let picker = Picker::new(&empty, Path::new("config.toml"));
        let screen = screen(&picker);
        assert!(screen.contains("no agents configured"), "{screen}");
        assert!(screen.contains("add one to config.toml"), "{screen}");
    }
}
