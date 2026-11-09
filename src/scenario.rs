use agent_client_protocol::v1::schema::{
    PermissionOption, SessionUpdate, StopReason, ToolCallUpdate,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Scenario {
    pub acts: Vec<Act>,

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
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Update(SessionUpdate),
    Permission(Permission),
    DelayMs(u64),
    Stop(StopReason),
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
