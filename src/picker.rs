use std::io;
use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::config::Config;

/// The agent picker shown on a bare launch.
pub struct Picker<'a> {
    names: Vec<&'a str>,
    commands: Vec<String>,
    selected: usize,
    path: &'a Path,
}

/// What a key press in the picker asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    Agent(&'a str),
    Quit,
}

impl<'a> Picker<'a> {
    pub fn new(config: &'a Config, path: &'a Path) -> Picker<'a> {
        let names: Vec<&str> = config.agents.keys().map(String::as_str).collect();
        let commands = config
            .agents
            .values()
            .map(|agent| {
                let mut words = vec![agent.command().display().to_string()];
                words.extend(agent.arguments().iter().cloned());
                words.join(" ")
            })
            .collect();
        let selected = config
            .default
            .as_deref()
            .and_then(|default| names.iter().position(|name| *name == default))
            .unwrap_or(0);
        Picker {
            names,
            commands,
            selected,
            path,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Option<Choice<'a>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Some(Choice::Quit),
            KeyCode::Esc => Some(Choice::Quit),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.names.len() {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => self
                .names
                .get(self.selected)
                .map(|name| Choice::Agent(name)),
            _ => None,
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let [title_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(frame.area());
        frame.render_widget(
            Paragraph::new(" pick an agent: Enter starts, Esc quits")
                .style(Style::default().add_modifier(Modifier::REVERSED)),
            title_area,
        );
        let lines: Vec<Line> = if self.names.is_empty() {
            vec![Line::from(format!(
                " no agents configured; add one to {}",
                self.path.display()
            ))]
        } else {
            self.names
                .iter()
                .zip(&self.commands)
                .enumerate()
                .map(|(index, (name, command))| {
                    let marker = if index == self.selected { ">" } else { " " };
                    let name = Span::styled(
                        format!("{marker} {name}  "),
                        if index == self.selected {
                            Style::default().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    );
                    let command = Span::styled(
                        command.clone(),
                        Style::default().add_modifier(Modifier::DIM),
                    );
                    Line::from(vec![name, command])
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(lines), list_area);
    }
}

/// Show the picker until an agent is chosen or the user quits.
pub fn pick(config: &Config, path: &Path) -> io::Result<Option<String>> {
    let mut picker = Picker::new(config, path);
    let mut terminal = ratatui::init();
    let result = loop {
        if let Err(error) = terminal.draw(|frame| picker.draw(frame)) {
            break Err(error);
        }
        let key = match crossterm::event::read() {
            Ok(Event::Key(key)) => key,
            Ok(_) => continue,
            Err(error) => break Err(error),
        };
        match picker.key(key) {
            Some(Choice::Agent(name)) => break Ok(Some(name.to_string())),
            Some(Choice::Quit) => break Ok(None),
            None => {}
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
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
}
