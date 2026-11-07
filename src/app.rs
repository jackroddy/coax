use std::io;

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::SessionId;
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::acp::{Event, Link};
use crate::session::Transcript;
use crate::ui;

pub struct App {
    pub transcript: Transcript,
    pub input: String,
    pub scroll: usize,
    pub agent: String,
    pub session: SessionId,
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
            Event::Permission { update, choice } => self.transcript.permission(*update, choice),
            Event::Done(Ok(stop)) => self.transcript.finished(stop),
            Event::Done(Err(error)) => {
                self.transcript.in_flight = false;
                self.transcript.notice(format!("prompt failed: {error}"));
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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

pub async fn run(mut link: Link) -> Result<(), Error> {
    let init = link.initialize().await?;
    let agent = init
        .agent_info
        .map(|info| info.name)
        .unwrap_or_else(|| "agent".to_string());
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let session = link.new_session(cwd).await?.session_id;

    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        scroll: 0,
        agent,
        session,
    };
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
