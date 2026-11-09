use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::SessionConfigOptionValue;
use agent_client_protocol::v1::schema::{
    InitializeResponse, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    SelectedPermissionOutcome, SessionId, ToolCallUpdate,
};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use decade_channel::{Channel, Message};
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;

use crate::acp::{Applied, Change, Event, Link};
use crate::session::Transcript;
use crate::settings::{Control, Settings};
use crate::ui;

const WHEEL_LINES: usize = 3;

/// How long a first ^c or ^d waits for the second.
const GUARD_WINDOW: Duration = Duration::from_millis(1500);

/// The key that armed the exit guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKey {
    CtrlC,
    CtrlD,
}

/// A first exit key, waiting for a second until `until`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    pub key: ExitKey,
    pub until: Instant,
}
const PAN_COLUMNS: isize = 4;

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
    pub show_plan: bool,
    pub settings: Settings,

    /// The header chips on screen, set by the last draw.
    pub chips: Vec<ChipArea>,
    pub hover: Option<ChipTarget>,
    pub menu: Option<Menu>,

    /// Where the open menu was drawn and the first of its rows shown, set by
    /// the last draw.
    pub menu_area: Option<Rect>,
    pub menu_first: usize,
    pub guard: Option<Guard>,

    /// How far each wide block is panned, by entry and block.
    pub pans: HashMap<(usize, usize), usize>,

    /// The wide blocks on screen, set by the last draw.
    pub wide: Vec<ui::WideBlock>,

    /// Rows a page scroll moves, set by the last draw.
    pub page: usize,

    /// Transcript lines at the last draw.
    pub lines_seen: usize,
    pub agent: String,
    pub session: SessionId,
    pub decade_pane: Option<String>,
    pub permission: Option<Permission>,
    pub rendered: ui::Rendered,
    pub cwd: PathBuf,
    auto_allow: bool,
}

pub struct Permission {
    pub title: String,
    pub options: Vec<PermissionOption>,
    pub update: Box<ToolCallUpdate>,
    reply: oneshot::Sender<RequestPermissionOutcome>,
}

/// A header chip on screen.
pub struct ChipArea {
    pub area: Rect,
    pub target: ChipTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipTarget {
    /// The setting at this index of `Settings::settings`.
    Setting(usize),

    /// The `⋯` chip, standing for the settings from this index on.
    Overflow(usize),
}

/// The picker box open under a chip.
pub struct Menu {
    pub target: MenuTarget,

    /// The highlighted row.
    pub cursor: usize,

    /// The column of the chip the box hangs from.
    pub anchor: u16,
    pub error: Option<String>,
}

pub enum MenuTarget {
    /// The setting with this id.
    Setting(String),

    /// The settings from this index on, which did not fit in the header.
    Overflow(usize),
}

pub enum Action {
    None,
    Change(Change),
    Prompt(String),
    Cancel,
    Quit,
}

impl App {
    pub fn event(&mut self, event: Event) {
        match event {
            Event::Update(update) => {
                self.settings.apply(&update);
                self.transcript.apply(*update);
            }
            Event::Tokens(usage) => self.settings.used(&usage),
            Event::Setting(Ok(applied)) => {
                match applied {
                    Applied::Options(options) => self.settings.set_options(options),
                    Applied::Mode(id) => self.settings.set_mode(&id),
                }
                self.menu = None;
            }
            Event::Setting(Err(error)) => match &mut self.menu {
                Some(menu) => menu.error = Some(error.to_string()),
                None => self.transcript.notice(format!("setting failed: {error}")),
            },
            Event::Permission {
                update,
                options,
                reply,
            } => {
                let title = update
                    .fields
                    .title
                    .clone()
                    .unwrap_or_else(|| update.tool_call_id.to_string());
                self.permission = Some(Permission {
                    title,
                    options,
                    update,
                    reply,
                });
                if self.auto_allow {
                    let allow = self.permission.as_ref().and_then(|p| {
                        p.options
                            .iter()
                            .position(|o| o.kind == PermissionOptionKind::AllowOnce)
                            .or((!p.options.is_empty()).then_some(0))
                    });
                    self.decide(allow);
                }
            }
            Event::Done(Ok(stop)) => self.transcript.finished(stop),
            Event::Done(Err(error)) => {
                self.transcript.in_flight = false;
                self.transcript.notice(format!("prompt failed: {error}"));
            }
        }
    }

    pub fn decade(&mut self, message: Message) {
        match message {
            Message::Welcome { pane, .. } => self.decade_pane = pane.or(Some(String::new())),
            Message::Error { reason, .. } => self.transcript.notice(format!("decade: {reason}")),
            Message::Closed => {
                self.decade_pane = None;
                self.transcript.notice("decade went away".to_string());
            }
            Message::Panes(_)
            | Message::Verb { .. }
            | Message::Show { .. }
            | Message::Highlight { .. }
            | Message::Clear { .. } => {}
        }
    }

    pub fn decide(&mut self, option: Option<usize>) {
        let Some(permission) = self.permission.take() else {
            return;
        };
        let (outcome, choice) = match option.and_then(|i| permission.options.get(i)) {
            Some(option) => (
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option.option_id.clone(),
                )),
                option.name.clone(),
            ),
            None => (RequestPermissionOutcome::Cancelled, "cancelled".to_string()),
        };
        let _ = permission.reply.send(outcome);
        self.transcript.permission(*permission.update, choice);
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Action {
        let position = Position::new(mouse.column, mouse.row);
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let pan = match mouse.kind {
            MouseEventKind::Moved => {
                self.hover = self.chip_at(position);
                return Action::None;
            }
            MouseEventKind::Down(MouseButton::Left) => return self.click(position),
            MouseEventKind::ScrollLeft => -PAN_COLUMNS,
            MouseEventKind::ScrollUp if shift => -PAN_COLUMNS,
            MouseEventKind::ScrollRight => PAN_COLUMNS,
            MouseEventKind::ScrollDown if shift => PAN_COLUMNS,
            MouseEventKind::ScrollUp => {
                self.scroll += WHEEL_LINES;
                return Action::None;
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(WHEEL_LINES);
                return Action::None;
            }
            _ => return Action::None,
        };
        if let Some(block) = self.wide.iter().find(|block| block.area.contains(position)) {
            let offset = self.pans.get(&block.key).copied().unwrap_or(0) as isize;
            let offset = (offset + pan).clamp(0, block.max as isize) as usize;
            self.pans.insert(block.key, offset);
        }
        Action::None
    }

    fn chip_at(&self, position: Position) -> Option<ChipTarget> {
        self.chips
            .iter()
            .find(|chip| chip.area.contains(position))
            .map(|chip| chip.target)
    }

    fn click(&mut self, position: Position) -> Action {
        if self.permission.is_some() {
            return Action::None;
        }
        if self.menu.is_some() {
            let Some(area) = self.menu_area.filter(|area| area.contains(position)) else {
                self.menu = None;
                return Action::None;
            };
            let row = (position.y.saturating_sub(area.y + 1)) as usize + self.menu_first;
            if position.y > area.y && row < self.menu_rows() {
                if let Some(menu) = &mut self.menu {
                    menu.cursor = row;
                }
                return self.choose();
            }
            return Action::None;
        }
        let Some(chip) = self.chips.iter().find(|chip| chip.area.contains(position)) else {
            return Action::None;
        };
        let anchor = chip.area.x;
        let settings = self.settings.settings();
        match chip.target {
            ChipTarget::Setting(index) => {
                let Some(setting) = settings.get(index) else {
                    return Action::None;
                };
                match &setting.control {
                    Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
                    Control::Choose(choices) => {
                        self.menu = Some(Menu {
                            target: MenuTarget::Setting(setting.id.clone()),
                            cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                            anchor,
                            error: None,
                        });
                        Action::None
                    }
                }
            }
            ChipTarget::Overflow(first) => {
                self.menu = Some(Menu {
                    target: MenuTarget::Overflow(first),
                    cursor: 0,
                    anchor,
                    error: None,
                });
                Action::None
            }
        }
    }

    /// How many rows the open menu has.
    pub fn menu_rows(&self) -> usize {
        let Some(menu) = &self.menu else {
            return 0;
        };
        let settings = self.settings.settings();
        match &menu.target {
            MenuTarget::Setting(id) => settings.iter().find(|setting| setting.id == *id).map_or(
                0,
                |setting| match &setting.control {
                    Control::Choose(choices) => choices.len(),
                    Control::Flip(_) => 0,
                },
            ),
            MenuTarget::Overflow(first) => settings.len().saturating_sub(*first),
        }
    }

    fn menu_key(&mut self, key: KeyEvent) -> Action {
        let rows = self.menu_rows();
        let Some(menu) = &mut self.menu else {
            return Action::None;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => menu.cursor = menu.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                menu.cursor = (menu.cursor + 1).min(rows.saturating_sub(1));
            }
            KeyCode::Enter => return self.choose(),
            KeyCode::Esc => self.menu = None,
            _ => {}
        }
        Action::None
    }

    /// Apply the highlighted row of the open menu.
    fn choose(&mut self) -> Action {
        let Some(menu) = &self.menu else {
            return Action::None;
        };
        let settings = self.settings.settings();
        match &menu.target {
            MenuTarget::Setting(id) => {
                let Some(setting) = settings.iter().find(|setting| setting.id == *id) else {
                    return Action::None;
                };
                let Control::Choose(choices) = &setting.control else {
                    return Action::None;
                };
                let Some(choice) = choices.get(menu.cursor) else {
                    return Action::None;
                };
                Action::Change(if setting.from_modes {
                    Change::Mode(choice.value.clone())
                } else {
                    Change::Config {
                        id: setting.id.clone(),
                        value: SessionConfigOptionValue::value_id(choice.value.clone()),
                    }
                })
            }
            MenuTarget::Overflow(first) => {
                let Some(setting) = settings.get(first + menu.cursor) else {
                    return Action::None;
                };
                match &setting.control {
                    Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
                    Control::Choose(choices) => {
                        self.menu = Some(Menu {
                            target: MenuTarget::Setting(setting.id.clone()),
                            cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                            anchor: menu.anchor,
                            error: None,
                        });
                        Action::None
                    }
                }
            }
        }
    }

    /// Handle a key pressed at `now`. Any key but the one that completes
    /// the exit guard disarms it.
    pub fn key(&mut self, key: KeyEvent, now: Instant) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let armed = self.guard.take().filter(|guard| guard.until > now);
        let exit = match key.code {
            KeyCode::Char('c') if ctrl => Some(ExitKey::CtrlC),
            KeyCode::Char('d') if ctrl && self.input.is_empty() => Some(ExitKey::CtrlD),
            _ => None,
        };
        if let Some(exit) = exit {
            if armed.is_some_and(|guard| guard.key == exit) {
                return Action::Quit;
            }
            let working = self.transcript.in_flight && self.permission.is_none();
            if exit == ExitKey::CtrlC && working {
                return Action::Cancel;
            }
            if exit == ExitKey::CtrlC && !self.input.is_empty() {
                self.input.clear();
                return Action::None;
            }
            self.guard = Some(Guard {
                key: exit,
                until: now + GUARD_WINDOW,
            });
            return Action::None;
        }
        if let Some(permission) = &self.permission {
            match key.code {
                KeyCode::Esc => self.decide(None),
                KeyCode::Char(digit) if digit.is_ascii_digit() => {
                    let index = digit.to_digit(10).unwrap() as usize;
                    if (1..=permission.options.len()).contains(&index) {
                        self.decide(Some(index - 1));
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        match key.code {
            KeyCode::Char('t') if ctrl => {
                self.show_plan = !self.show_plan;
                Action::None
            }
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                Action::None
            }
            KeyCode::Esc if self.transcript.in_flight => Action::Cancel,
            KeyCode::Enter => {
                if self.transcript.in_flight || self.input.trim().is_empty() {
                    return Action::None;
                }
                let text = std::mem::take(&mut self.input);
                self.transcript.prompt(text.clone());
                self.scroll = 0;
                Action::Prompt(text)
            }
            KeyCode::Backspace => {
                self.input.pop();
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll += self.page;
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page);
                Action::None
            }
            KeyCode::End => {
                self.scroll = 0;
                Action::None
            }
            KeyCode::Char(ch) if !ctrl => {
                self.input.push(ch);
                Action::None
            }
            _ => Action::None,
        }
    }
}

/// The terminal's events, read on a thread of their own.
pub type Keys = UnboundedReceiver<io::Result<TermEvent>>;

/// Which session a run opens on.
pub enum Start {
    New,
    Load(SessionId),
}

/// What the user asked for at launch.
pub struct Opening {
    pub start: Start,
    pub first_prompt: Option<String>,
    pub auto_allow: bool,
    pub keys: Keys,
}

fn flip(id: &str, on: bool) -> Change {
    Change::Config {
        id: id.to_string(),
        value: SessionConfigOptionValue::boolean(!on),
    }
}

pub async fn run(
    mut link: Link,
    init: InitializeResponse,
    name: Option<String>,
    opening: Opening,
) -> Result<(), Error> {
    let Opening {
        start,
        first_prompt,
        auto_allow,
        mut keys,
    } = opening;
    let agent = name
        .or(init.agent_info.map(|info| info.name))
        .unwrap_or_else(|| "agent".to_string());
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let mut settings = Settings::default();
    let session = match start {
        Start::New => {
            let response = link.new_session(cwd.clone()).await?;
            settings.open(response.config_options, response.modes);
            response.session_id
        }
        Start::Load(id) => {
            let response = link.load_session(id.clone(), cwd.clone()).await?;
            settings.open(response.config_options, response.modes);
            id
        }
    };

    let (decade, mut decade_messages) = connect_decade();
    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        scroll: 0,
        show_plan: false,
        settings,
        chips: Vec::new(),
        hover: None,
        menu: None,
        menu_area: None,
        menu_first: 0,
        guard: None,
        pans: HashMap::new(),
        wide: Vec::new(),
        page: 1,
        lines_seen: 0,
        agent,
        session,
        decade_pane: decade.as_ref().map(|_| String::new()),
        permission: None,
        rendered: ui::Rendered::default(),
        cwd,
        auto_allow,
    };
    // a loaded session's history arrives before its load response
    while let Ok(event) = link.events.try_recv() {
        app.event(event);
    }
    if let Some(text) = first_prompt {
        app.transcript.prompt(text.clone());
        link.prompt(app.session.clone(), text)?;
    }
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
    let restore_terminal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
        restore_terminal(info);
    }));
    let result = loop {
        if let Err(error) = terminal.draw(|frame| ui::draw(frame, &mut app)) {
            break Err(Error::into_internal_error(error));
        }
        let expiry = app.guard.map(|guard| guard.until);
        let action = tokio::select! {
            event = link.events.recv() => match event {
                Some(event) => {
                    app.event(event);
                    Action::None
                }
                None => break Ok(()),
            },
            key = keys.recv() => match key {
                Some(Ok(TermEvent::Key(key))) => app.key(key, Instant::now()),
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    app.mouse(mouse)
                }
                Some(Ok(_)) => Action::None,
                Some(Err(error)) => break Err(Error::into_internal_error(error)),
                None => break Ok(()),
            },
            () = async { tokio::time::sleep_until(expiry.unwrap().into()).await }, if expiry.is_some() => {
                app.guard = None;
                Action::None
            },
            message = decade_messages.recv() => {
                match message {
                    Some(message) => app.decade(message),
                    None => decade_messages = mpsc::unbounded_channel().1,
                }
                Action::None
            },
        };
        let outcome = match action {
            Action::None => Ok(()),
            Action::Quit => break Ok(()),
            Action::Prompt(text) => link.prompt(app.session.clone(), text),
            Action::Cancel => link.cancel(app.session.clone()),
            Action::Change(change) => link.change(app.session.clone(), change),
        };
        if let Err(error) = outcome {
            break Err(error);
        }
    };
    restore();
    result
}

pub fn restore() {
    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

fn connect_decade() -> (Option<Channel>, UnboundedReceiver<Message>) {
    let (tx, rx) = mpsc::unbounded_channel();
    match Channel::from_env("coax", &[], &[], None) {
        Some(Ok((channel, messages))) => {
            std::thread::spawn(move || {
                for message in messages {
                    if tx.send(message).is_err() {
                        return;
                    }
                }
            });
            (Some(channel), rx)
        }
        Some(Err(_)) | None => (None, rx),
    }
}

pub fn key_thread() -> Keys {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            let event = crossterm::event::read();
            let failed = event.is_err();
            if tx.send(event).is_err() || failed {
                return;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use crossterm::event::MouseEventKind;
    use ratatui::layout::Rect;

    use super::*;
    use crate::ui::WideBlock;

    fn app() -> App {
        App {
            transcript: Transcript::default(),
            input: String::new(),
            scroll: 0,
            show_plan: false,
            settings: Settings::default(),
            chips: Vec::new(),
            hover: None,
            menu: None,
            menu_area: None,
            menu_first: 0,
            guard: None,
            pans: HashMap::new(),
            wide: vec![WideBlock {
                area: Rect::new(2, 5, 20, 3),
                key: (1, 0),
                max: 6,
            }],
            page: 1,
            lines_seen: 0,
            agent: String::new(),
            session: SessionId::new("s"),
            decade_pane: None,
            permission: None,
            rendered: ui::Rendered::default(),
            cwd: PathBuf::new(),
            auto_allow: false,
        }
    }

    fn wheel(kind: MouseEventKind, modifiers: KeyModifiers, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[test]
    fn a_horizontal_wheel_over_a_wide_block_pans_it_four_columns_and_stops_at_the_ends() {
        let mut app = app();
        app.mouse(wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 4);
        app.mouse(wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 6);
        for _ in 0..3 {
            app.mouse(wheel(MouseEventKind::ScrollLeft, KeyModifiers::NONE, 3, 6));
        }
        assert_eq!(app.pans[&(1, 0)], 0);
    }

    #[test]
    fn shift_with_the_wheel_pans_and_a_plain_wheel_scrolls_the_conversation() {
        let mut app = app();
        app.mouse(wheel(MouseEventKind::ScrollDown, KeyModifiers::SHIFT, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 4);
        assert_eq!(app.scroll, 0);
        app.mouse(wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.scroll, 3);
        assert_eq!(app.pans[&(1, 0)], 4);
    }

    #[test]
    fn panning_away_from_every_wide_block_does_nothing() {
        let mut app = app();
        app.mouse(wheel(
            MouseEventKind::ScrollRight,
            KeyModifiers::NONE,
            40,
            20,
        ));
        assert!(app.pans.is_empty());
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn open_permission(app: &mut App) {
        let (reply, _answer) = oneshot::channel();
        app.permission = Some(Permission {
            title: "Write".to_string(),
            options: Vec::new(),
            update: Box::new(ToolCallUpdate::new("t1", Default::default())),
            reply,
        });
    }

    #[test]
    fn ctrl_c_while_the_agent_is_working_cancels_the_turn() {
        let mut app = app();
        app.transcript.in_flight = true;
        assert!(matches!(app.key(ctrl('c'), Instant::now()), Action::Cancel));
        assert!(app.guard.is_none());
    }

    #[test]
    fn ctrl_c_with_text_in_the_prompt_clears_it() {
        let mut app = app();
        app.input = "half a thought".to_string();
        assert!(matches!(app.key(ctrl('c'), Instant::now()), Action::None));
        assert!(app.input.is_empty());
        assert!(app.guard.is_none());
    }

    #[test]
    fn ctrl_c_on_an_idle_empty_prompt_arms_and_a_second_one_inside_the_window_quits() {
        let mut app = app();
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(ExitKey::CtrlC));
        let second = start + Duration::from_millis(1400);
        assert!(matches!(app.key(ctrl('c'), second), Action::Quit));
    }

    #[test]
    fn a_second_ctrl_c_after_the_window_arms_again_instead_of_quitting() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        let late = start + Duration::from_millis(1600);
        assert!(matches!(app.key(ctrl('c'), late), Action::None));
        assert_eq!(
            app.guard.map(|guard| guard.until),
            Some(late + GUARD_WINDOW)
        );
    }

    #[test]
    fn ctrl_c_with_the_permission_box_open_arms_and_a_second_quits() {
        let mut app = app();
        app.transcript.in_flight = true;
        open_permission(&mut app);
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert!(app.guard.is_some());
        assert!(matches!(app.key(ctrl('c'), start), Action::Quit));
    }

    #[test]
    fn ctrl_d_on_an_empty_prompt_arms_its_own_guard_and_a_second_quits() {
        let mut app = app();
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('d'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(ExitKey::CtrlD));
        assert!(matches!(app.key(ctrl('d'), start), Action::Quit));
    }

    #[test]
    fn ctrl_d_with_text_in_the_prompt_does_nothing() {
        let mut app = app();
        app.input = "text".to_string();
        assert!(matches!(app.key(ctrl('d'), Instant::now()), Action::None));
        assert!(app.guard.is_none());
        assert_eq!(app.input, "text");
    }

    #[test]
    fn the_other_exit_key_does_not_complete_the_guard() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        assert!(matches!(app.key(ctrl('d'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(ExitKey::CtrlD));
    }

    #[test]
    fn any_other_key_disarms_the_guard() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        app.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), start);
        assert!(app.guard.is_none());
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert!(app.guard.is_some());
    }

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::scenario::Scenario;

    fn with_settings() -> App {
        let scenario = Scenario::parse(include_str!("../scenarios/resume.json")).unwrap();
        let mut app = app();
        app.wide.clear();
        app.settings.open(Some(scenario.config_options), None);
        app
    }

    fn header(app: &mut App, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }

    #[test]
    fn the_header_shows_the_chips_the_context_and_the_tokens() {
        use agent_client_protocol::v1::schema::{Cost, SessionUpdate, Usage, UsageUpdate};

        let mut app = with_settings();
        let update = UsageUpdate::new(28_481, 1_000_000).cost(Cost::new(0.136, "USD"));
        app.settings.apply(&SessionUpdate::UsageUpdate(update));
        app.settings
            .used(&Usage::new(28_485, 12, 4).cached_read_tokens(28_469));
        let text = header(&mut app, 140);
        assert!(
            text.contains(" Auto   Opus   Effort Default   Fast off "),
            "{text}"
        );
        assert!(text.contains("ctx 28k/1.0M 3%"), "{text}");
        assert!(text.contains("↑28k ↓4 $0.14"), "{text}");
        assert!(text.contains("idle"), "{text}");
        assert_eq!(app.chips.len(), 4);
    }

    #[test]
    fn a_narrow_header_keeps_the_mode_chip_and_the_state() {
        let mut app = with_settings();
        let text = header(&mut app, 50);
        assert!(text.contains(" Auto "), "{text}");
        assert!(text.contains(" ⋯ "), "{text}");
        assert!(text.contains("idle"), "{text}");
        assert!(!text.contains("session"), "{text}");
    }

    fn click(app: &mut App, column: u16, row: u16) -> Action {
        app.mouse(wheel(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            column,
            row,
        ))
    }

    #[test]
    fn a_click_on_a_select_chip_opens_its_menu_and_enter_applies_the_highlighted_choice() {
        let mut app = with_settings();
        header(&mut app, 140);
        let model = app.chips[1].area;
        assert!(matches!(
            click(&mut app, model.x + 1, model.y),
            Action::None
        ));
        assert_eq!(app.menu_rows(), 3);
        let enter = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        app.key(enter, Instant::now());
        let action = app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Instant::now(),
        );
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!(id, "model");
        assert_eq!(value.as_value_id().map(|id| &*id.0), Some("sonnet"));
    }

    #[test]
    fn a_click_on_a_boolean_chip_flips_it_without_a_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let fast = app.chips[3].area;
        let action = click(&mut app, fast.x, fast.y);
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!((id.as_str(), value.as_bool()), ("fast", Some(true)));
        assert!(app.menu.is_none());
    }

    #[test]
    fn escape_and_a_click_elsewhere_close_the_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        assert!(app.menu.is_some());
        app.key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(app.menu.is_none());
        click(&mut app, mode.x, mode.y);
        click(&mut app, 100, 20);
        assert!(app.menu.is_none());
    }

    #[test]
    fn an_error_from_the_agent_shows_in_the_open_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        app.event(Event::Setting(Err(Error::invalid_params())));
        assert!(app.menu.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn the_overflow_chip_lists_the_settings_that_did_not_fit() {
        let mut app = with_settings();
        header(&mut app, 30);
        let overflow = app.chips.last().unwrap().area;
        click(&mut app, overflow.x, overflow.y);
        assert!(app.menu_rows() >= 2);
    }
}
