use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::{
    InitializeResponse, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    SelectedPermissionOutcome, SessionId, ToolCallUpdate,
};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent, MouseEventKind,
};
use decade_channel::{Channel, Message};
use ratatui::layout::Position;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;

use crate::acp::{Event, Link};
use crate::session::Transcript;
use crate::ui;

const WHEEL_LINES: usize = 3;
const PAN_COLUMNS: isize = 4;

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
    pub show_plan: bool,

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

pub enum Action {
    None,
    Prompt(String),
    Cancel,
    Quit,
}

impl App {
    pub fn event(&mut self, event: Event) {
        match event {
            Event::Update(update) => self.transcript.apply(*update),
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

    pub fn mouse(&mut self, mouse: MouseEvent) {
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let pan = match mouse.kind {
            MouseEventKind::ScrollLeft => -PAN_COLUMNS,
            MouseEventKind::ScrollUp if shift => -PAN_COLUMNS,
            MouseEventKind::ScrollRight => PAN_COLUMNS,
            MouseEventKind::ScrollDown if shift => PAN_COLUMNS,
            MouseEventKind::ScrollUp => {
                self.scroll += WHEEL_LINES;
                return;
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(WHEEL_LINES);
                return;
            }
            _ => return,
        };
        let position = Position::new(mouse.column, mouse.row);
        if let Some(block) = self.wide.iter().find(|block| block.area.contains(position)) {
            let offset = self.pans.get(&block.key).copied().unwrap_or(0) as isize;
            let offset = (offset + pan).clamp(0, block.max as isize) as usize;
            self.pans.insert(block.key, offset);
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(permission) = &self.permission {
            match key.code {
                KeyCode::Char('c') if ctrl => return Action::Quit,
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
        match key.code {
            KeyCode::Char('c') if ctrl => Action::Quit,
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
    let session = match start {
        Start::New => link.new_session(cwd.clone()).await?.session_id,
        Start::Load(id) => {
            link.load_session(id.clone(), cwd.clone()).await?;
            id
        }
    };

    let (decade, mut decade_messages) = connect_decade();
    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        scroll: 0,
        show_plan: false,
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
        let action = tokio::select! {
            event = link.events.recv() => match event {
                Some(event) => {
                    app.event(event);
                    Action::None
                }
                None => break Ok(()),
            },
            key = keys.recv() => match key {
                Some(Ok(TermEvent::Key(key))) => app.key(key),
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    app.mouse(mouse);
                    Action::None
                }
                Some(Ok(_)) => Action::None,
                Some(Err(error)) => break Err(Error::into_internal_error(error)),
                None => break Ok(()),
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
}
