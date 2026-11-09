use std::io;
use std::path::PathBuf;

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionId, ToolCallUpdate,
};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use decade_channel::{Channel, Message};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;

use crate::acp::{Event, Link};
use crate::session::Transcript;
use crate::ui;

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
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
                self.scroll += 10;
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
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

pub async fn run(
    mut link: Link,
    name: Option<String>,
    first_prompt: Option<String>,
    auto_allow: bool,
) -> Result<(), Error> {
    let init = link.initialize().await?;
    let agent = name
        .or(init.agent_info.map(|info| info.name))
        .unwrap_or_else(|| "agent".to_string());
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let session = link.new_session(cwd.clone()).await?.session_id;

    let (decade, mut decade_messages) = connect_decade();
    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        scroll: 0,
        agent,
        session,
        decade_pane: decade.as_ref().map(|_| String::new()),
        permission: None,
        rendered: ui::Rendered::default(),
        cwd,
        auto_allow,
    };
    if let Some(text) = first_prompt {
        app.transcript.prompt(text.clone());
        link.prompt(app.session.clone(), text)?;
    }
    let mut keys = key_thread();
    let mut terminal = ratatui::init();
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
    ratatui::restore();
    result
}

fn connect_decade() -> (Option<Channel>, UnboundedReceiver<Message>) {
    let (tx, rx) = mpsc::unbounded_channel();
    match Channel::from_env("coax", &[], &[]) {
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

fn key_thread() -> UnboundedReceiver<io::Result<TermEvent>> {
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
