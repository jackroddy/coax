use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::SessionConfigOptionValue;
use agent_client_protocol::v1::schema::{
    InitializeResponse, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    SelectedPermissionOutcome, SessionId, ToolCallId, ToolCallUpdate,
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
use crate::clipboard;
use crate::command::{self, Candidate, Kind};
use crate::session::{Entry, Transcript};
use crate::settings::{Control, Setting, Settings};
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
/// How often the working spinner steps.
const SPIN_INTERVAL: Duration = Duration::from_millis(150);

const PAN_COLUMNS: isize = 4;

/// How long a block's copy glyph says `copied`.
const COPIED_FOR: Duration = Duration::from_secs(1);

/// A block being panned by the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    key: (usize, usize),

    /// The column the button went down on.
    from: u16,

    /// How far the block was panned then.
    offset: usize,
    max: usize,
}

/// The offset of a block dragged from column `from` to column `to`: the
/// block follows the pointer, so dragging right shows what is to the left.
fn dragged(offset: usize, from: u16, to: u16, max: usize) -> usize {
    (offset as isize - (to as isize - from as isize)).clamp(0, max as isize) as usize
}

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
    pub show_plan: bool,

    /// The highlighted entry of the command list under the prompt.
    pub command_cursor: usize,

    /// Whether Esc has closed the command list for the text typed so far.
    pub command_closed: bool,

    /// An agent command waiting for a yes before it is sent.
    pub confirm: Option<Confirm>,

    /// The subagent calls shown unfolded.
    pub expanded: HashSet<ToolCallId>,

    /// The subagent blocks on screen, set by the last draw.
    pub folds: Vec<ui::FoldArea>,
    pub settings: Settings,

    /// The header chips on screen, set by the last draw.
    pub chips: Vec<ChipArea>,
    pub hover: Option<ChipTarget>,

    /// The working spinner's frame, stepped while the agent works.
    pub spinner: usize,
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

    /// The block the pointer is panning, from a press until the release.
    pub drag: Option<Drag>,

    /// The copy glyphs on screen, set by the last draw.
    pub copies: Vec<ui::CopyArea>,

    /// The block whose copy glyph the pointer is over.
    pub hover_copy: Option<(usize, usize)>,

    /// The block just copied and when its glyph stops saying so.
    pub copied: Option<((usize, usize), Instant)>,

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

/// An agent command, as the box that asks before sending it shows it.
pub struct Confirm {
    pub name: String,
    pub description: String,
    pub input: String,
}

impl Confirm {
    /// The prompt it sends: `/name` and then the input.
    pub fn text(&self) -> String {
        if self.input.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.input)
        }
    }
}

/// The picker box open under a chip.
pub struct Menu {
    pub target: MenuTarget,

    /// The highlighted row.
    pub cursor: usize,

    /// The column of the chip the box hangs from.
    pub anchor: u16,
    pub error: Option<String>,

    /// The value of a model choice waiting for a second choice to confirm it.
    pub confirm: Option<String>,
}

pub enum MenuTarget {
    /// The setting with this id.
    Setting(String),

    /// The settings from this index on, which did not fit in the header.
    Overflow(usize),
}

pub enum Action {
    None,

    /// Put this text on the system clipboard.
    Copy(String),
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
                self.hover_copy = self.copy_at(position);
                return Action::None;
            }
            MouseEventKind::Down(MouseButton::Left) => return self.click(position),
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = self.drag {
                    let offset = dragged(drag.offset, drag.from, position.x, drag.max);
                    self.pans.insert(drag.key, offset);
                }
                return Action::None;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag = None;
                return Action::None;
            }
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

    fn copy_at(&self, position: Position) -> Option<(usize, usize)> {
        self.copies
            .iter()
            .find(|copy| copy.area.contains(position))
            .map(|copy| copy.key)
    }

    fn chip_at(&self, position: Position) -> Option<ChipTarget> {
        self.chips
            .iter()
            .find(|chip| chip.area.contains(position))
            .map(|chip| chip.target)
    }

    fn click(&mut self, position: Position) -> Action {
        if self.permission.is_some() || self.confirm.is_some() {
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
            if let Some(key) = self.copy_at(position) {
                let Some(text) = self.rendered.copy_text(key) else {
                    return Action::None;
                };
                let text = text.to_string();
                self.copied = Some((key, Instant::now() + COPIED_FOR));
                return Action::Copy(text);
            }
            if let Some(fold) = self.folds.iter().find(|fold| fold.area.contains(position)) {
                let id = fold.id.clone();
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
                return Action::None;
            }
            if let Some(block) = self.wide.iter().find(|block| block.area.contains(position)) {
                self.drag = Some(Drag {
                    key: block.key,
                    from: position.x,
                    offset: self.pans.get(&block.key).copied().unwrap_or(0),
                    max: block.max,
                });
            }
            return Action::None;
        };
        let anchor = chip.area.x;
        match chip.target {
            ChipTarget::Setting(index) => self.activate(index, anchor),
            ChipTarget::Overflow(first) => {
                self.menu = Some(Menu {
                    target: MenuTarget::Overflow(first),
                    cursor: 0,
                    anchor,
                    error: None,
                    confirm: None,
                });
                Action::None
            }
        }
    }

    /// Whether a turn has been played, so a model change costs a reread.
    fn has_conversation(&self) -> bool {
        self.transcript
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::User(_) | Entry::Agent(_)))
    }

    /// Do what a click on the chip of setting `index` does: flip a yes or no
    /// setting, or open a choice's box under column `anchor`. The model waits
    /// while the agent works.
    fn activate(&mut self, index: usize, anchor: u16) -> Action {
        let settings = self.settings.settings();
        let Some(setting) = settings.get(index) else {
            return Action::None;
        };
        match &setting.control {
            Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
            Control::Choose(_) if setting.model && self.transcript.in_flight => Action::None,
            Control::Choose(choices) => {
                self.menu = Some(Menu {
                    target: MenuTarget::Setting(setting.id.clone()),
                    cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                    anchor,
                    error: None,
                    confirm: None,
                });
                Action::None
            }
        }
    }

    /// The commands the prompt's text can complete to, while a name is being
    /// typed.
    pub fn popup(&self) -> Vec<Candidate> {
        let busy = self.permission.is_some() || self.menu.is_some() || self.confirm.is_some();
        if busy || self.command_closed || !command::naming(&self.input) {
            return Vec::new();
        }
        command::matches(
            &self.input,
            &self.settings.settings(),
            &self.settings.commands,
        )
    }

    /// Enter on a command: fill the highlighted entry of the list, or run
    /// the command when the prompt already holds it.
    fn enter_command(&mut self) -> Action {
        let settings = self.settings.settings();
        let list = self.popup();
        let entry = match list.get(self.command_cursor.min(list.len().saturating_sub(1))) {
            Some(entry) if self.input.trim_end() != entry.fill.trim_end() => {
                self.input = entry.fill.clone();
                self.command_cursor = 0;
                return Action::None;
            }
            Some(entry) => Some(entry.clone()),
            None => self.exact(&settings),
        };
        let Some(entry) = entry else {
            return Action::None;
        };
        match entry.kind {
            Kind::Coax(index) => {
                self.input.clear();
                let anchor = self
                    .chips
                    .iter()
                    .find(|chip| chip.target == ChipTarget::Setting(index))
                    .map_or(0, |chip| chip.area.x);
                self.activate(index, anchor)
            }
            Kind::Agent { name } => {
                if self.transcript.in_flight {
                    return Action::None;
                }
                let input = match command::parse(&self.input) {
                    command::Typed::Agent { input, .. } => input.trim_end().to_string(),
                    _ => String::new(),
                };
                self.confirm = Some(Confirm {
                    name,
                    description: entry.description,
                    input,
                });
                Action::None
            }
        }
    }

    /// The command named exactly by the prompt's text, if there is one.
    fn exact(&self, settings: &[Setting]) -> Option<Candidate> {
        let name = match command::parse(&self.input) {
            command::Typed::Coax { name } | command::Typed::Agent { name, .. } => name,
            command::Typed::Plain => return None,
        };
        command::matches(&self.input, settings, &self.settings.commands)
            .into_iter()
            .find(|candidate| candidate.fill.trim_start_matches('/').trim_end() == name)
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Enter => {
                let Some(confirm) = self.confirm.take() else {
                    return Action::None;
                };
                let text = confirm.text();
                self.input.clear();
                self.transcript.prompt(text.clone());
                self.scroll = 0;
                Action::Prompt(text)
            }
            KeyCode::Esc => {
                self.confirm = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn edited(&mut self) {
        self.command_cursor = 0;
        self.command_closed = false;
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
                let change = if setting.from_modes {
                    Change::Mode(choice.value.clone())
                } else {
                    Change::Config {
                        id: setting.id.clone(),
                        value: SessionConfigOptionValue::value_id(choice.value.clone()),
                    }
                };
                if !(setting.model && self.has_conversation()) {
                    return Action::Change(change);
                }
                // a model change in a conversation waits for a second choice
                // of the same model
                if choice.current {
                    self.menu = None;
                } else if menu.confirm.as_deref() == Some(choice.value.as_str()) {
                    return Action::Change(change);
                } else if let Some(menu) = &mut self.menu {
                    menu.confirm = Some(choice.value.clone());
                }
                Action::None
            }
            MenuTarget::Overflow(first) => {
                let Some(setting) = settings.get(first + menu.cursor) else {
                    return Action::None;
                };
                match &setting.control {
                    Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
                    Control::Choose(_) if setting.model && self.transcript.in_flight => {
                        Action::None
                    }
                    Control::Choose(choices) => {
                        self.menu = Some(Menu {
                            target: MenuTarget::Setting(setting.id.clone()),
                            cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                            anchor: menu.anchor,
                            error: None,
                            confirm: None,
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
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        let list = self.popup().len();
        match key.code {
            KeyCode::Up if list > 0 => {
                self.command_cursor = self.command_cursor.saturating_sub(1);
                return Action::None;
            }
            KeyCode::Down if list > 0 => {
                self.command_cursor = (self.command_cursor + 1).min(list - 1);
                return Action::None;
            }
            KeyCode::Esc if list > 0 => {
                self.command_closed = true;
                return Action::None;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Char('t') if ctrl => {
                self.show_plan = !self.show_plan;
                Action::None
            }
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.edited();
                Action::None
            }
            KeyCode::Esc if self.transcript.in_flight => Action::Cancel,
            KeyCode::Enter if !matches!(command::parse(&self.input), command::Typed::Plain) => {
                self.enter_command()
            }
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
                self.edited();
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
                self.edited();
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
        command_cursor: 0,
        command_closed: false,
        confirm: None,
        expanded: HashSet::new(),
        folds: Vec::new(),
        settings,
        chips: Vec::new(),
        hover: None,
        spinner: 0,
        menu: None,
        menu_area: None,
        menu_first: 0,
        guard: None,
        pans: HashMap::new(),
        wide: Vec::new(),
        drag: None,
        copies: Vec::new(),
        hover_copy: None,
        copied: None,
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
    let mut spin = tokio::time::interval(SPIN_INTERVAL);
    spin.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        if let Err(error) = terminal.draw(|frame| ui::draw(frame, &mut app)) {
            break Err(Error::into_internal_error(error));
        }
        let expiry = app.guard.map(|guard| guard.until);
        let copied_until = app.copied.map(|(_, until)| until);
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
            _ = spin.tick(), if app.transcript.in_flight => {
                app.spinner = app.spinner.wrapping_add(1);
                Action::None
            },
            () = async { tokio::time::sleep_until(copied_until.unwrap().into()).await }, if copied_until.is_some() => {
                app.copied = None;
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
            Action::Copy(text) => {
                let mut out = io::stdout();
                out.write_all(clipboard::osc52(&text).as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(Error::into_internal_error)
            }
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
            command_cursor: 0,
            command_closed: false,
            confirm: None,
            expanded: HashSet::new(),
            folds: Vec::new(),
            settings: Settings::default(),
            chips: Vec::new(),
            hover: None,
            spinner: 0,
            menu: None,
            menu_area: None,
            menu_first: 0,
            guard: None,
            pans: HashMap::new(),
            drag: None,
            copies: Vec::new(),
            hover_copy: None,
            copied: None,
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

    #[test]
    fn dragging_a_block_pans_it_column_for_column_against_the_pointer() {
        assert_eq!(dragged(10, 50, 47, 40), 13);
        assert_eq!(dragged(10, 50, 54, 40), 6);
        assert_eq!(dragged(10, 50, 50, 40), 10);
    }

    #[test]
    fn a_drag_stops_at_the_ends_of_the_block() {
        assert_eq!(dragged(2, 50, 60, 40), 0);
        assert_eq!(dragged(30, 50, 20, 40), 40);
    }

    fn drag_event(kind: MouseEventKind, column: u16, row: u16) -> Action {
        let mut app = app();
        app.mouse(wheel(kind, KeyModifiers::NONE, column, row))
    }

    #[test]
    fn pressing_on_a_wide_block_and_dragging_pans_it_until_the_release() {
        let mut app = app();
        let at = |app: &mut App, kind, column| {
            app.mouse(wheel(kind, KeyModifiers::NONE, column, 6));
        };
        at(&mut app, MouseEventKind::Down(MouseButton::Left), 10);
        at(&mut app, MouseEventKind::Drag(MouseButton::Left), 8);
        assert_eq!(app.pans[&(1, 0)], 2);
        at(&mut app, MouseEventKind::Drag(MouseButton::Left), 5);
        assert_eq!(app.pans[&(1, 0)], 5);
        at(&mut app, MouseEventKind::Up(MouseButton::Left), 5);
        at(&mut app, MouseEventKind::Drag(MouseButton::Left), 0);
        assert_eq!(app.pans[&(1, 0)], 5);
        assert!(app.drag.is_none());
    }

    #[test]
    fn dragging_with_no_press_on_a_block_pans_nothing() {
        let mut app = app();
        app.mouse(wheel(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            40,
            20,
        ));
        app.mouse(wheel(
            MouseEventKind::Drag(MouseButton::Left),
            KeyModifiers::NONE,
            30,
            20,
        ));
        assert!(app.pans.is_empty());
        assert!(matches!(
            drag_event(MouseEventKind::Up(MouseButton::Left), 1, 1),
            Action::None
        ));
    }

    /// An app whose first entry is agent text with one code block, drawn
    /// once so the copy glyph has a place on screen.
    fn with_a_code_block() -> App {
        let mut app = app();
        app.wide.clear();
        app.transcript.entries.push(Entry::Agent(
            "```rs\nfn main() {\n    run();\n}\n```".to_string(),
        ));
        header(&mut app, 80);
        app
    }

    #[test]
    fn a_click_on_the_copy_glyph_copies_the_whole_block_text_and_marks_it_copied() {
        let mut app = with_a_code_block();
        assert_eq!(app.copies.len(), 1);
        let glyph = app.copies[0].area;
        let Action::Copy(text) = click(&mut app, glyph.x + glyph.width - 1, glyph.y) else {
            panic!("expected a copy");
        };
        assert_eq!(text, "fn main() {\n    run();\n}");
        assert!(matches!(app.copied, Some(((0, 0), _))));
    }

    #[test]
    fn a_click_elsewhere_in_a_code_block_copies_nothing() {
        let mut app = with_a_code_block();
        let glyph = app.copies[0].area;
        assert!(matches!(
            click(&mut app, glyph.x, glyph.y + 1),
            Action::None
        ));
        assert!(app.copied.is_none());
    }

    #[test]
    fn the_copy_glyph_is_accent_under_the_pointer_and_says_copied_after_a_click() {
        let mut app = with_a_code_block();
        let glyph = app.copies[0].area;
        let last = glyph.x + glyph.width - 1;
        let cell = |app: &mut App, x: u16| {
            let buffer = header_buffer(app, 80);
            (
                buffer[(x, glyph.y)].symbol().to_string(),
                buffer[(x, glyph.y)].fg,
            )
        };
        let (symbol, colour) = cell(&mut app, last);
        assert_eq!(symbol, "\u{f0c5}");
        assert_ne!(colour, crate::palette::get().accent);
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            last,
            glyph.y,
        ));
        assert_eq!(cell(&mut app, last).1, crate::palette::get().accent);
        click(&mut app, last, glyph.y);
        let buffer = header_buffer(&mut app, 80);
        let row: String = (0..80)
            .map(|x| buffer[(x, glyph.y)].symbol().to_string())
            .collect();
        assert!(row.contains("copied"), "{row}");
    }

    #[test]
    fn a_click_on_an_edit_or_run_call_unfolds_it_and_a_second_click_folds_it() {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{Diff, ToolCallContent, ToolCallStatus, ToolKind};

        let mut app = with_settings();
        app.wide.clear();
        let mut diff = Diff::new("src/main.rs", "new line");
        diff.old_text = Some("old line".to_string());
        let tool = Tool {
            id: "e1".into(),
            title: "Edit src/main.rs".to_string(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::Completed,
            locations: Vec::new(),
            content: vec![ToolCallContent::Diff(diff)],
            children: Vec::new(),
        };
        app.transcript.entries.push(Entry::Tool(tool));
        let rows = |app: &mut App| header_rows_all(app, 80);
        assert!(!rows(&mut app).contains("old line"));
        let fold = &app.folds[0];
        let (x, y) = (fold.area.x, fold.area.y);
        click(&mut app, x, y);
        assert!(rows(&mut app).contains("- old line"));
        click(&mut app, x, y);
        assert!(!rows(&mut app).contains("old line"));
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

    fn header_buffer(app: &mut App, width: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The two header rows, one string each.
    fn header_rows(app: &mut App, width: u16) -> [String; 2] {
        let buffer = header_buffer(app, width);
        [0, 1].map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
    }

    fn header_rows_all(app: &mut App, width: u16) -> String {
        let buffer = header_buffer(app, width);
        (0..30)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn header(app: &mut App, width: u16) -> String {
        header_rows(app, width).join("\n")
    }

    #[test]
    fn the_header_shows_the_chips_the_context_and_the_tokens() {
        use agent_client_protocol::v1::schema::{Cost, SessionUpdate, Usage, UsageUpdate};

        let mut app = with_settings();
        app.agent = "claude".to_string();
        let update = UsageUpdate::new(28_481, 1_000_000).cost(Cost::new(0.136, "USD"));
        app.settings.apply(&SessionUpdate::UsageUpdate(update));
        app.settings
            .used(&Usage::new(28_485, 12, 4).cached_read_tokens(28_469));
        let [identity, chips] = header_rows(&mut app, 140);
        assert!(
            identity.starts_with("\u{e0b6} coax \u{e0b0} claude \u{e0b4}"),
            "{identity}"
        );
        assert!(identity.contains(" ctx 28k/1.0M 3% "), "{identity}");
        assert!(identity.contains(" ↑28k ↓4 $0.14 "), "{identity}");
        assert!(identity.contains(" idle "), "{identity}");
        assert!(!identity.contains("session"), "{identity}");
        for label in [" Auto ", " Opus ", " Effort Default ", " Fast off "] {
            assert!(chips.contains(label), "{chips}");
        }
        assert_eq!(app.chips.len(), 4);
    }

    #[test]
    fn a_narrow_header_keeps_the_mode_chip_and_the_state() {
        let mut app = with_settings();
        let [identity, chips] = header_rows(&mut app, 30);
        assert!(chips.contains(" Auto "), "{chips}");
        assert!(chips.contains(" ⋯ "), "{chips}");
        assert!(identity.contains(" idle "), "{identity}");
        assert!(!identity.contains("ctx"), "{identity}");
    }

    #[test]
    fn the_header_is_two_lines_with_no_rule_under_them() {
        let mut app = with_settings();
        let buffer = header_buffer(&mut app, 100);
        let third: String = (0..100)
            .map(|x| buffer[(x, 2)].symbol().to_string())
            .collect();
        assert!(!third.contains('─'), "{third}");
    }

    #[test]
    fn the_separators_on_screen_join_the_colours_of_their_neighbours() {
        let mut app = with_settings();
        let buffer = header_buffer(&mut app, 100);
        let join = &buffer[(7, 0)];
        assert_eq!(join.symbol(), "\u{e0b0}");
        assert_eq!(join.fg, buffer[(2, 0)].bg);
        assert_eq!(join.bg, buffer[(8, 0)].bg);
    }

    #[test]
    fn the_overflow_chip_replaces_the_chips_that_do_not_fit_on_the_second_line() {
        let mut app = with_settings();
        let [_, chips] = header_rows(&mut app, 40);
        assert!(chips.contains(" ⋯ "), "{chips}");
        assert!(!chips.contains("Fast off"), "{chips}");
        assert_eq!(
            app.chips.last().unwrap().target,
            ChipTarget::Overflow(app.chips.len() - 1)
        );
    }

    #[test]
    fn a_hovered_chip_turns_its_text_accent_without_an_underline() {
        let mut app = with_settings();
        header_buffer(&mut app, 140);
        let area = app.chips[1].area;
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            area.x + 1,
            area.y,
        ));
        let buffer = header_buffer(&mut app, 140);
        let cell = &buffer[(area.x + 1, area.y)];
        assert_eq!(cell.fg, crate::palette::get().accent);
        assert!(!cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        let other = &buffer[(app.chips[2].area.x + 1, area.y)];
        assert_ne!(other.fg, crate::palette::get().accent);
    }

    #[test]
    fn the_working_segment_shows_the_spinner_frame() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        let frame = |app: &mut App| header_rows(app, 100)[0].clone();
        assert!(frame(&mut app).contains(" ◐ working "));
        app.spinner = 1;
        assert!(frame(&mut app).contains(" ◓ working "));
        app.spinner = 5;
        assert!(frame(&mut app).contains(" ◓ working "));
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

    #[test]
    fn a_click_on_a_subagent_block_unfolds_it_and_a_second_click_folds_it() {
        let mut app = app();
        app.wide.clear();
        app.folds = vec![ui::FoldArea {
            area: Rect::new(0, 4, 60, 2),
            id: ToolCallId::new("t1"),
        }];
        click(&mut app, 3, 5);
        assert!(app.expanded.contains(&ToolCallId::new("t1")));
        click(&mut app, 3, 4);
        assert!(app.expanded.is_empty());
        click(&mut app, 3, 9);
        assert!(app.expanded.is_empty());
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn down() -> KeyEvent {
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    }

    /// An app past its first turn with the model menu open on `Opus`.
    fn in_a_conversation_with_the_model_menu_open() -> App {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        header(&mut app, 140);
        let model = app.chips[1].area;
        click(&mut app, model.x, model.y);
        assert!(app.menu.is_some());
        app
    }

    #[test]
    fn choosing_another_model_in_a_conversation_asks_for_the_same_choice_again() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert_eq!(
            app.menu.as_ref().unwrap().confirm.as_deref(),
            Some("sonnet")
        );
        let action = app.key(enter(), Instant::now());
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!(id, "model");
        assert_eq!(value.as_value_id().map(|id| &*id.0), Some("sonnet"));
    }

    #[test]
    fn choosing_a_different_model_than_the_one_waiting_moves_the_warning() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(down(), Instant::now());
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert_eq!(app.menu.as_ref().unwrap().confirm.as_deref(), Some("haiku"));
    }

    #[test]
    fn choosing_the_current_model_or_escape_backs_out_of_the_warning() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert!(app.menu.is_none());

        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_model_choice_before_the_first_turn_applies_at_once() {
        let mut app = with_settings();
        header(&mut app, 140);
        let model = app.chips[1].area;
        click(&mut app, model.x, model.y);
        app.key(down(), Instant::now());
        assert!(matches!(
            app.key(enter(), Instant::now()),
            Action::Change(_)
        ));
    }

    #[test]
    fn other_settings_switch_at_once_in_a_conversation() {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        header(&mut app, 140);
        let effort = app.chips[2].area;
        click(&mut app, effort.x, effort.y);
        app.key(down(), Instant::now());
        assert!(matches!(
            app.key(enter(), Instant::now()),
            Action::Change(_)
        ));
    }

    #[test]
    fn the_model_chip_does_nothing_while_the_agent_is_working() {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        header(&mut app, 140);
        let model = app.chips[1].area;
        assert!(matches!(click(&mut app, model.x, model.y), Action::None));
        assert!(app.menu.is_none());
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        assert!(app.menu.is_some());
    }

    #[test]
    fn the_warning_shows_under_the_choices_with_the_context_size() {
        use agent_client_protocol::v1::schema::{SessionUpdate, UsageUpdate};

        let mut app = in_a_conversation_with_the_model_menu_open();
        app.settings
            .apply(&SessionUpdate::UsageUpdate(UsageUpdate::new(
                412_000, 1_000_000,
            )));
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..30)
            .map(|y| {
                (0..140)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("rereads the conversation (412k tokens) through"),
            "{screen}"
        );
    }

    fn with_commands() -> App {
        use agent_client_protocol::v1::schema::{AvailableCommandsUpdate, SessionUpdate};

        let scenario = Scenario::parse(include_str!("../scenarios/resume.json")).unwrap();
        let mut app = with_settings();
        app.settings.apply(&SessionUpdate::AvailableCommandsUpdate(
            AvailableCommandsUpdate::new(scenario.commands),
        ));
        app
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            app.key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                Instant::now(),
            );
        }
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now())
    }

    #[test]
    fn a_slash_prefix_lists_the_coax_commands_that_start_with_it() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        let names: Vec<String> = app.popup().into_iter().map(|entry| entry.fill).collect();
        assert_eq!(names, ["/mode", "/model"]);
        type_text(&mut app, "dex");
        assert!(app.popup().is_empty());
    }

    #[test]
    fn enter_on_a_listed_coax_command_fills_it_and_a_second_enter_opens_its_box() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "/mode");
        assert!(app.menu.is_none());
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(matches!(
            &app.menu.as_ref().unwrap().target,
            MenuTarget::Setting(id) if id == "mode"
        ));
        assert!(app.input.is_empty());
    }

    #[test]
    fn the_arrows_move_within_the_list_before_enter_fills_an_entry() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, "/model");
    }

    #[test]
    fn a_slash_command_never_sends_a_prompt() {
        for text in ["/mode", "/model", "/mo", "/", "/nothing", "/fast"] {
            let mut app = with_commands();
            type_text(&mut app, text);
            for _ in 0..3 {
                let action = press(&mut app, KeyCode::Enter);
                assert!(!matches!(action, Action::Prompt(_)), "{text}");
            }
        }
    }

    #[test]
    fn an_unknown_slash_command_does_nothing_and_keeps_the_text() {
        let mut app = with_commands();
        type_text(&mut app, "/nothing");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "/nothing");
        assert!(app.menu.is_none());
    }

    #[test]
    fn escape_closes_the_list_without_cancelling_the_turn() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "/m");
        assert!(!app.popup().is_empty());
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::None));
        assert!(app.popup().is_empty());
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::Cancel));
    }

    #[test]
    fn the_model_command_does_nothing_while_the_agent_works() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "/model");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_double_slash_command_asks_before_sending_and_enter_sends_it_as_a_prompt() {
        let mut app = with_commands();
        type_text(&mut app, "//compact");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        let confirm = app.confirm.as_ref().unwrap();
        assert_eq!(confirm.text(), "/compact");
        assert_eq!(
            confirm.description,
            "Summarise the conversation to free context"
        );
        let Action::Prompt(text) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a prompt");
        };
        assert_eq!(text, "/compact");
        assert!(app.confirm.is_none());
        assert!(app.input.is_empty());
    }

    #[test]
    fn the_input_after_an_agent_commands_name_goes_into_the_prompt_it_sends() {
        let mut app = with_commands();
        type_text(&mut app, "//review the parser");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.confirm.as_ref().unwrap().text(), "/review the parser");
        let Action::Prompt(text) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a prompt");
        };
        assert_eq!(text, "/review the parser");
    }

    #[test]
    fn escape_backs_out_of_the_confirm_and_keeps_the_text() {
        let mut app = with_commands();
        type_text(&mut app, "//compact");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert!(app.confirm.is_none());
        assert_eq!(app.input, "//compact");
    }

    #[test]
    fn an_agent_command_is_not_sent_while_the_agent_works() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "//compact");
        press(&mut app, KeyCode::Enter);
        assert!(app.confirm.is_none());
    }

    #[test]
    fn an_unknown_agent_command_is_not_sent() {
        let mut app = with_commands();
        type_text(&mut app, "//nothing");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(app.confirm.is_none());
    }
}
