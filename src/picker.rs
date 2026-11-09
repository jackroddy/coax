use std::io;
use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::config::Config;
use crate::palette;
use crate::ui;

/// The agent picker shown on a bare launch.
pub struct Picker<'a> {
    names: Vec<&'a str>,
    commands: Vec<String>,
    default: Option<usize>,
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
            selected: default.unwrap_or(0),
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
        let [header_area, list_area, footer_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        ui::title_bar(frame, header_area, "pick an agent", Line::default());
        let block = ui::pane("agents", true);
        let inner = block.inner(list_area);
        frame.render_widget(block, list_area);
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
        let hints = if self.names.is_empty() {
            "esc quit"
        } else {
            "j/k move   enter start   esc quit"
        };
        frame.render_widget(Paragraph::new(hints), footer_area);
    }

    fn row(&self, index: usize, width: usize) -> Line<'static> {
        let name_width = self
            .names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0);
        let chosen = index == self.selected;
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
