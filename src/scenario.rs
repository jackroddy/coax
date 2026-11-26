use agent_client_protocol::v1::schema::{
    AvailableCommand, ElicitationAction, ElicitationSchema, PermissionOption, SessionConfigOption,
    SessionModeState, SessionUpdate, StopReason, ToolCallUpdate, Usage,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Scenario {
    pub acts: Vec<Act>,

    /// The settings the agent offers a new or loaded session, as config
    /// options. A `set_config_option` request changes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_options: Vec<SessionConfigOption>,

    /// The slash commands the agent advertises after a session opens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<AvailableCommand>,

    /// The older modes API, offered next to `config_options`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,

    /// Earlier sessions the agent lists and can load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SavedSession>,

    /// The person's side, which `coax --input` types into coax.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<InputStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputStep {
    Type {
        r#type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        per_char_ms: Option<u64>,
    },

    /// A key by name, such as `Enter`, `Esc`, `Up` or `j`.
    Key {
        key: String,
    },
    Delay {
        delay_ms: u64,
    },
    Wait {
        wait: Wait,
    },
    Move {
        r#move: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    Click {
        click: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    Double {
        double: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    Drag {
        drag: Drag,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
}

/// Text on the screen a mouse step aims at: a string, taken at the middle of
/// its first match, or the `nth` match taken `at` one end. A negative `nth`
/// counts back from the last match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
    Text(String),
    Match {
        text: String,
        #[serde(default)]
        nth: i64,
        #[serde(default)]
        at: Anchor,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Start,
    #[default]
    Middle,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drag {
    pub from: Target,
    pub to: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wait {
    /// Until the agent's turn has ended.
    Idle,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: String,
    pub title: String,

    /// An ISO 8601 timestamp.
    pub updated_at: String,

    /// The steps replayed when the session is loaded. Only `update` steps
    /// play.
    pub history: Vec<Step>,
}

#[derive(Serialize, Deserialize)]
pub struct Act {
    pub steps: Vec<Step>,

    /// The token usage the prompt response reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Update(SessionUpdate),
    Permission(Permission),

    /// Ask for a form, as an `elicitation/create` request in form mode.
    Elicitation(Elicitation),
    DelayMs(u64),
    Stop(StopReason),

    /// End the process the agent runs in. Use it only with the fake agent
    /// binary.
    Exit(Exit),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Exit {
    pub code: i32,

    /// Text written to stderr first.
    #[serde(default)]
    pub stderr: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Permission {
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
    #[serde(default)]
    pub expect: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Elicitation {
    pub message: String,
    pub requested_schema: ElicitationSchema,

    /// The answer the step expects. A different one is reported on stderr.
    #[serde(default)]
    pub expect: Option<ElicitationAction>,
}

impl Scenario {
    pub fn parse(json: &str) -> Result<Scenario, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_without_input_still_parses_with_an_empty_input() {
        let scenario = Scenario::parse(r#"{"acts": []}"#).unwrap();
        assert!(scenario.input.is_empty());
    }

    #[test]
    fn each_kind_of_input_step_parses_from_its_own_key() {
        let scenario = Scenario::parse(
            r#"{"acts": [], "input": [
                {"type": "hi", "per_char_ms": 80},
                {"type": "yo"},
                {"key": "Enter"},
                {"delay_ms": 300},
                {"wait": "idle"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            scenario.input,
            [
                InputStep::Type {
                    r#type: "hi".to_string(),
                    per_char_ms: Some(80),
                },
                InputStep::Type {
                    r#type: "yo".to_string(),
                    per_char_ms: None,
                },
                InputStep::Key {
                    key: "Enter".to_string(),
                },
                InputStep::Delay { delay_ms: 300 },
                InputStep::Wait { wait: Wait::Idle },
            ]
        );
    }

    #[test]
    fn each_kind_of_mouse_step_parses_with_a_plain_or_a_placed_target() {
        let scenario = Scenario::parse(
            r#"{"acts": [], "input": [
                {"move": "mark"},
                {"click": {"text": "raw", "nth": 1}, "ms": 300},
                {"double": {"text": "blah", "at": "end"}},
                {"drag": {"from": {"text": "first", "at": "start"}, "to": "blah."}}
            ]}"#,
        )
        .unwrap();
        let placed = |text: &str, nth, at| Target::Match {
            text: text.to_string(),
            nth,
            at,
        };
        assert_eq!(
            scenario.input,
            [
                InputStep::Move {
                    r#move: Target::Text("mark".to_string()),
                    ms: None,
                },
                InputStep::Click {
                    click: placed("raw", 1, Anchor::Middle),
                    ms: Some(300),
                },
                InputStep::Double {
                    double: placed("blah", 0, Anchor::End),
                    ms: None,
                },
                InputStep::Drag {
                    drag: Drag {
                        from: placed("first", 0, Anchor::Start),
                        to: Target::Text("blah.".to_string()),
                    },
                    ms: None,
                },
            ]
        );
    }
}
