use agent_client_protocol::v1::schema::AvailableCommand;

use crate::settings::Setting;

/// What the prompt's text starts with.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed<'a> {
    Plain,

    /// `/name`: a coax command. Anything after the name is ignored.
    Coax {
        name: &'a str,
    },

    /// `//name input`: one of the agent's commands, with the input after it.
    Agent {
        name: &'a str,
        input: &'a str,
    },
}

pub fn parse(text: &str) -> Typed<'_> {
    if let Some(rest) = text.strip_prefix("//") {
        let (name, input) = split_name(rest);
        return Typed::Agent { name, input };
    }
    match text.strip_prefix('/') {
        Some(rest) => Typed::Coax {
            name: split_name(rest).0,
        },
        None => Typed::Plain,
    }
}

fn split_name(rest: &str) -> (&str, &str) {
    match rest.split_once(char::is_whitespace) {
        Some((name, input)) => (name, input.trim_start()),
        None => (rest, ""),
    }
}

/// What to say when `text` names no command, or `None` for plain text.
pub fn unknown(text: &str) -> Option<String> {
    match parse(text) {
        Typed::Plain => None,
        Typed::Coax { name } => Some(format!("no command /{name}")),
        Typed::Agent { name, .. } => Some(format!("no agent command //{name}")),
    }
}

/// Whether the cursor is still inside the command's name, so a list of
/// matches makes sense.
pub fn naming(text: &str) -> bool {
    text.starts_with('/') && !text.contains(char::is_whitespace)
}

/// What a setting is called as a command: lowercase, spaces as `-`.
pub fn command_name(setting: &str) -> String {
    setting.trim().to_lowercase().replace(' ', "-")
}

/// A command the prompt can complete to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The text it fills into the prompt, `/mode` or `//compact`.
    pub fill: String,
    pub description: String,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The setting at this index of `Settings::settings`.
    Coax(usize),
    Agent {
        name: String,
    },
}

/// The commands whose names start with what is typed, coax's for `/` and the
/// agent's for `//`.
pub fn matches(text: &str, settings: &[Setting], agent: &[AvailableCommand]) -> Vec<Candidate> {
    match parse(text) {
        Typed::Plain => Vec::new(),
        Typed::Coax { name } => {
            let typed = name.to_lowercase();
            settings
                .iter()
                .enumerate()
                .filter(|(_, setting)| command_name(&setting.name).starts_with(&typed))
                .map(|(index, setting)| Candidate {
                    fill: format!("/{}", command_name(&setting.name)),
                    description: setting
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("now {}", setting.label)),
                    kind: Kind::Coax(index),
                })
                .collect()
        }
        Typed::Agent { name, .. } => {
            let typed = name.to_lowercase();
            agent
                .iter()
                .map(|command| (command.name.trim_start_matches('/'), command))
                .filter(|(name, _)| name.to_lowercase().starts_with(&typed))
                .map(|(name, command)| Candidate {
                    // a command that takes input leaves room to type it
                    fill: format!("//{name}{}", if command.input.is_some() { " " } else { "" }),
                    description: command.description.clone(),
                    kind: Kind::Agent {
                        name: name.to_string(),
                    },
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{AvailableCommandInput, UnstructuredCommandInput};

    use super::*;
    use crate::settings::Control;

    fn setting(name: &str) -> Setting {
        Setting {
            id: name.to_string(),
            name: name.to_string(),
            label: "value".to_string(),
            value: "value".to_string(),
            description: None,
            control: Control::Flip(false),
            unguarded: false,
            from_modes: false,
            model: false,
        }
    }

    #[test]
    fn a_single_slash_is_a_coax_command_and_ignores_what_follows_the_name() {
        assert_eq!(parse("/mode"), Typed::Coax { name: "mode" });
        assert_eq!(parse("/mode now"), Typed::Coax { name: "mode" });
        assert_eq!(parse("/"), Typed::Coax { name: "" });
    }

    #[test]
    fn a_double_slash_is_an_agent_command_with_the_input_after_its_name() {
        assert_eq!(
            parse("//review the parser"),
            Typed::Agent {
                name: "review",
                input: "the parser"
            }
        );
        assert_eq!(
            parse("//compact"),
            Typed::Agent {
                name: "compact",
                input: ""
            }
        );
        assert_eq!(
            parse("//"),
            Typed::Agent {
                name: "",
                input: ""
            }
        );
    }

    #[test]
    fn text_that_does_not_start_with_a_slash_is_plain() {
        assert_eq!(parse("hello"), Typed::Plain);
        assert_eq!(parse(" /mode"), Typed::Plain);
        assert_eq!(parse(""), Typed::Plain);
    }

    #[test]
    fn a_setting_name_becomes_a_lowercase_command_name_joined_with_dashes() {
        assert_eq!(command_name("Fast Mode"), "fast-mode");
        assert_eq!(command_name("effort"), "effort");
    }

    #[test]
    fn a_prefix_matches_every_coax_command_that_starts_with_it() {
        let settings = [setting("Mode"), setting("Model"), setting("Fast")];
        let names: Vec<String> = matches("/mo", &settings, &[])
            .into_iter()
            .map(|candidate| candidate.fill)
            .collect();
        assert_eq!(names, ["/mode", "/model"]);
        assert_eq!(matches("/", &settings, &[]).len(), 3);
        assert!(matches("/x", &settings, &[]).is_empty());
        assert_eq!(matches("/MO", &settings, &[]).len(), 2);
    }

    #[test]
    fn a_double_slash_prefix_matches_the_agents_commands_and_a_hint_leaves_room_for_input() {
        let agent = [
            AvailableCommand::new("compact", "Summarise the conversation"),
            AvailableCommand::new("review", "Review code").input(
                AvailableCommandInput::Unstructured(UnstructuredCommandInput::new(
                    "what to review",
                )),
            ),
        ];
        let found = matches("//re", &[], &agent);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].fill, "//review ");
        assert_eq!(found[0].description, "Review code");
        assert_eq!(matches("//", &[], &agent).len(), 2);
        assert!(matches("/co", &[], &agent).is_empty());
    }

    #[test]
    fn the_list_is_open_only_while_the_name_is_being_typed() {
        assert!(naming("/mo"));
        assert!(naming("//"));
        assert!(!naming("/mode "));
        assert!(!naming("//review the parser"));
        assert!(!naming("hello"));
    }

    #[test]
    fn a_slash_name_is_unknown_in_the_words_of_its_kind() {
        assert_eq!(unknown("/nope").as_deref(), Some("no command /nope"));
        assert_eq!(
            unknown("//nope with input").as_deref(),
            Some("no agent command //nope")
        );
        assert_eq!(unknown("hello"), None);
    }
}
