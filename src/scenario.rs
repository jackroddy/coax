use agent_client_protocol::v1::schema::{
    AvailableCommand, PermissionOption, SessionConfigOption, SessionModeState, SessionUpdate,
    StopReason, ToolCallUpdate, Usage,
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

impl Scenario {
    pub fn parse(json: &str) -> Result<Scenario, serde_json::Error> {
        serde_json::from_str(json)
    }
}
