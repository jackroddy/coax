use std::io;
use std::time::{Duration, Instant};

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionId, SessionUpdate, ToolCall, ToolCallUpdate,
};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use decade_channel::code::{MARK, SHOW};
use decade_channel::{Channel, Message};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;

use crate::acp::{Event, Link};
use crate::follow::{self, Action as Follow};
use crate::session::Transcript;
use crate::ui;

const FOLLOW_QUIET: Duration = Duration::from_millis(150);

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
    pub agent: String,
    pub session: SessionId,
    pub decade_pane: Option<String>,
    pub permission: Option<Permission>,
    pub rendered: ui::Rendered,
    auto_allow: bool,
    calls: Vec<ToolCall>,
    pending: Vec<Follow>,
    flush_at: Option<Instant>,
}

pub struct Permission {
    pub title: String,
    pub options: Vec<PermissionOption>,
    update: Box<ToolCallUpdate>,
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
            Event::Update(update) => {
                self.track(&update);
                let actions = follow::from_update(&update, &self.calls);
                self.follow(actions);
                self.transcript.apply(*update);
            }
            Event::FileRead { path, line } => self.follow(vec![follow::from_read(path, line)]),
            Event::Permission {
                update,
                options,
                reply,
            } => {
                if !self
                    .calls
                    .iter()
                    .any(|call| call.tool_call_id == update.tool_call_id)
                {
                    let mut call = ToolCall::new(
                        update.tool_call_id.clone(),
                        update.fields.title.clone().unwrap_or_default(),
                    );
                    call.update(update.fields.clone());
                    self.calls.push(call);
                }
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

    fn track(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::ToolCall(call) => self.calls.push(call.clone()),
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(call) = self
                    .calls
                    .iter_mut()
                    .find(|call| call.tool_call_id == update.tool_call_id)
                {
                    call.update(update.fields.clone());
                }
            }
            _ => {}
        }
    }

    fn follow(&mut self, actions: Vec<Follow>) {
        if actions.is_empty() || self.decade_pane.is_none() {
            return;
        }
        self.pending = actions;
        self.flush_at = Some(Instant::now() + FOLLOW_QUIET);
    }

    pub fn flush(&mut self, decade: &mut Option<Channel>) {
        self.flush_at = None;
        let Some(channel) = decade else {
            return;
        };
        for action in std::mem::take(&mut self.pending) {
            let sent = match action {
                Follow::Show(show) => channel.send("code", SHOW, show),
                Follow::Mark(mark) => channel.send("code", MARK, mark),
            };
            if let Err(error) = sent {
                self.transcript.notice(format!("decade: {error}"));
                *decade = None;
                self.decade_pane = None;
                return;
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
            Message::Panes(_) | Message::Verb { .. } => {}
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
    first_prompt: Option<String>,
    auto_allow: bool,
) -> Result<(), Error> {
    let init = link.initialize().await?;
    let agent = init
        .agent_info
        .map(|info| info.name)
        .unwrap_or_else(|| "agent".to_string());
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let session = link.new_session(cwd).await?.session_id;

    let (mut decade, mut decade_messages) = connect_decade();
    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        scroll: 0,
        agent,
        session,
        decade_pane: decade.as_ref().map(|_| String::new()),
        permission: None,
        rendered: ui::Rendered::default(),
        auto_allow,
        calls: Vec::new(),
        pending: Vec::new(),
        flush_at: None,
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
            _ = tokio::time::sleep_until(app.flush_at.map(Into::into).unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600))), if app.flush_at.is_some() => {
                app.flush(&mut decade);
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
    match Channel::from_env("coax", &[]) {
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
