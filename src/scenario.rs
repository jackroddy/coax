use agent_client_protocol::v1::schema::{
    PermissionOption, SessionUpdate, StopReason, ToolCallUpdate,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Scenario {
    pub acts: Vec<Act>,
}

#[derive(Serialize, Deserialize)]
pub struct Act {
    pub steps: Vec<Step>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Update(SessionUpdate),
    Permission(Permission),
    DelayMs(u64),
    Stop(StopReason),
}

#[derive(Serialize, Deserialize)]
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
