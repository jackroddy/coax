type Meta = serde_json::Map<String, serde_json::Value>;

use agent_client_protocol::v1::schema::{
    ContentBlock, PlanEntry, SessionUpdate, StopReason, ToolCall, ToolCallContent, ToolCallId,
    ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolKind,
};

pub struct Tool {
    pub id: ToolCallId,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolCallStatus,
    pub locations: Vec<ToolCallLocation>,
    pub content: Vec<ToolCallContent>,

    /// What a subagent started by this call did, in arrival order.
    pub children: Vec<Child>,
}

impl Tool {
    /// How many tool calls the subagent made.
    pub fn tool_count(&self) -> usize {
        self.children
            .iter()
            .filter(|child| matches!(child, Child::Tool(_)))
            .count()
    }
}

pub enum Child {
    Tool(Tool),
    Text(String),
    Thought(String),
}

pub enum Entry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(Tool),
    Permission { title: String, choice: String },
    Notice(String),
}

#[derive(Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    pub plan: Vec<PlanEntry>,
    pub in_flight: bool,
    pub last_stop: Option<StopReason>,
}

impl Transcript {
    pub fn prompt(&mut self, text: String) {
        self.entries.push(Entry::User(text));
        self.in_flight = true;
    }

    pub fn finished(&mut self, stop: StopReason) {
        self.in_flight = false;
        self.last_stop = Some(stop);
    }

    pub fn permission(&mut self, update: ToolCallUpdate, choice: String) {
        let id = update.tool_call_id.clone();
        self.update_tool(update);
        let title = find_tool(&mut self.entries, &id)
            .map(|tool| tool.title.clone())
            .unwrap_or_else(|| id.to_string());
        self.entries.push(Entry::Permission { title, choice });
    }

    pub fn notice(&mut self, text: String) {
        self.entries.push(Entry::Notice(text));
    }

    pub fn apply(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                let text = content_text(&chunk.content);
                if let Some(parent) = self.parent(chunk.meta.as_ref()) {
                    match parent.children.last_mut() {
                        Some(Child::Text(existing)) => existing.push_str(&text),
                        _ => parent.children.push(Child::Text(text)),
                    }
                    return;
                }
                match self.entries.last_mut() {
                    Some(Entry::Agent(existing)) => existing.push_str(&text),
                    _ => self.entries.push(Entry::Agent(text)),
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                let text = content_text(&chunk.content);
                if let Some(parent) = self.parent(chunk.meta.as_ref()) {
                    match parent.children.last_mut() {
                        Some(Child::Thought(existing)) => existing.push_str(&text),
                        _ => parent.children.push(Child::Thought(text)),
                    }
                    return;
                }
                match self.entries.last_mut() {
                    Some(Entry::Thought(existing)) => existing.push_str(&text),
                    _ => self.entries.push(Entry::Thought(text)),
                }
            }
            SessionUpdate::UserMessageChunk(chunk) => {
                let text = content_text(&chunk.content);
                match self.entries.last_mut() {
                    Some(Entry::User(existing)) => existing.push_str(&text),
                    _ => self.entries.push(Entry::User(text)),
                }
            }
            SessionUpdate::ToolCall(call) => match self.parent(call.meta.as_ref()) {
                Some(parent) => parent.children.push(Child::Tool(tool(call))),
                None => self.entries.push(Entry::Tool(tool(call))),
            },
            SessionUpdate::ToolCallUpdate(update) => self.update_tool(update),
            SessionUpdate::Plan(plan) => self.plan = plan.entries,
            SessionUpdate::Notice(notice) => {
                let mut text = notice.title;
                if let Some(description) = notice.description {
                    text.push_str(": ");
                    text.push_str(&description);
                }
                self.entries.push(Entry::Notice(text));
            }
            _ => {}
        }
    }

    /// The tool call that a subagent's output, tagged by `meta`, belongs to.
    fn parent(&mut self, meta: Option<&Meta>) -> Option<&mut Tool> {
        let id = parent_tool_call(meta)?;
        find_tool(&mut self.entries, &id)
    }

    fn update_tool(&mut self, update: ToolCallUpdate) {
        let ToolCallUpdate {
            tool_call_id,
            fields,
            meta,
            ..
        } = update;
        let Some(tool) = find_tool(&mut self.entries, &tool_call_id) else {
            let mut call = ToolCall::new(tool_call_id, fields.title.clone().unwrap_or_default());
            call.update(fields);
            match self.parent(meta.as_ref()) {
                Some(parent) => parent.children.push(Child::Tool(tool(call))),
                None => self.entries.push(Entry::Tool(tool(call))),
            }
            return;
        };
        if let Some(title) = fields.title {
            tool.title = title;
        }
        if let Some(kind) = fields.kind {
            tool.kind = kind;
        }
        if let Some(status) = fields.status {
            tool.status = status;
        }
        if let Some(locations) = fields.locations {
            tool.locations = locations;
        }
        if let Some(content) = fields.content {
            tool.content = content;
        }
    }
}

/// The latest tool call named `id`, at the top level or inside a subagent.
fn find_tool<'a>(entries: &'a mut [Entry], id: &ToolCallId) -> Option<&'a mut Tool> {
    for entry in entries.iter_mut().rev() {
        if let Entry::Tool(tool) = entry
            && let Some(found) = within(tool, id)
        {
            return Some(found);
        }
    }
    None
}

fn within<'a>(tool: &'a mut Tool, id: &ToolCallId) -> Option<&'a mut Tool> {
    if tool.id == *id {
        return Some(tool);
    }
    for child in tool.children.iter_mut().rev() {
        if let Child::Tool(tool) = child
            && let Some(found) = within(tool, id)
        {
            return Some(found);
        }
    }
    None
}

/// The tool call that produced an update, read from the Claude adapter's
/// `_meta.claudeCode.parentToolUseId` tag. The adapter puts it on the tool
/// calls and the text a subagent produces, naming the Agent or Task call that
/// started the subagent.
pub fn parent_tool_call(meta: Option<&Meta>) -> Option<ToolCallId> {
    let id = meta?.get("claudeCode")?.get("parentToolUseId")?.as_str()?;
    Some(ToolCallId::new(id))
}

fn tool(call: ToolCall) -> Tool {
    Tool {
        id: call.tool_call_id,
        title: call.title,
        kind: call.kind,
        status: call.status,
        locations: call.locations,
        content: call.content,
        children: Vec::new(),
    }
}

pub fn content_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(_) => "[image]".to_string(),
        ContentBlock::Audio(_) => "[audio]".to_string(),
        ContentBlock::ResourceLink(link) => format!("[{}]", link.uri),
        ContentBlock::Resource(_) => "[resource]".to_string(),
        _ => "[content]".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::{ContentChunk, ToolCallUpdateFields};

    fn chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::from(text))
    }

    #[test]
    fn consecutive_message_chunks_join_into_one_agent_entry() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::AgentMessageChunk(chunk("hel")));
        transcript.apply(SessionUpdate::AgentMessageChunk(chunk("lo")));
        assert_eq!(transcript.entries.len(), 1);
        assert!(matches!(&transcript.entries[0], Entry::Agent(text) if text == "hello"));
    }

    #[test]
    fn a_thought_between_message_chunks_starts_a_new_agent_entry() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::AgentMessageChunk(chunk("one")));
        transcript.apply(SessionUpdate::AgentThoughtChunk(chunk("hmm")));
        transcript.apply(SessionUpdate::AgentMessageChunk(chunk("two")));
        assert_eq!(transcript.entries.len(), 3);
    }

    #[test]
    fn a_tool_call_update_changes_the_call_it_names_in_place() {
        let mut transcript = Transcript::default();
        let call = ToolCall::new("c1", "Read main.rs").kind(ToolKind::Read);
        transcript.apply(SessionUpdate::ToolCall(call));
        transcript.apply(SessionUpdate::AgentMessageChunk(chunk("reading")));
        transcript.apply(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "c1",
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .locations(vec![ToolCallLocation::new("src/main.rs").line(3)]),
        )));
        assert_eq!(transcript.entries.len(), 2);
        let Entry::Tool(tool) = &transcript.entries[0] else {
            panic!("first entry is not the tool call");
        };
        assert_eq!(tool.status, ToolCallStatus::Completed);
        assert_eq!(tool.locations[0].line, Some(3));
        assert_eq!(tool.title, "Read main.rs");
    }

    #[test]
    fn an_update_for_an_unknown_call_creates_the_call() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "ghost",
            ToolCallUpdateFields::new().title("Late call"),
        )));
        assert!(matches!(&transcript.entries[0], Entry::Tool(tool) if tool.title == "Late call"));
    }

    #[test]
    fn a_plan_replaces_the_previous_plan() {
        use agent_client_protocol::v1::schema::{Plan, PlanEntryPriority, PlanEntryStatus};
        let mut transcript = Transcript::default();
        let entry =
            |text: &str| PlanEntry::new(text, PlanEntryPriority::Medium, PlanEntryStatus::Pending);
        transcript.apply(SessionUpdate::Plan(Plan::new(vec![entry("a"), entry("b")])));
        transcript.apply(SessionUpdate::Plan(Plan::new(vec![entry("c")])));
        assert_eq!(transcript.plan.len(), 1);
        assert_eq!(transcript.plan[0].content, "c");
    }

    fn tagged(parent: &str) -> Meta {
        let mut meta = Meta::new();
        meta.insert(
            "claudeCode".to_string(),
            serde_json::json!({ "parentToolUseId": parent }),
        );
        meta
    }

    #[test]
    fn the_adapters_parent_tag_names_the_task_call() {
        assert_eq!(
            parent_tool_call(Some(&tagged("t1"))),
            Some(ToolCallId::new("t1"))
        );
        assert_eq!(parent_tool_call(Some(&Meta::new())), None);
        assert_eq!(parent_tool_call(None), None);
    }

    #[test]
    fn a_tagged_chunk_for_an_unknown_parent_falls_back_to_the_top_level() {
        let mut transcript = Transcript::default();
        let mut orphan = chunk("lost");
        orphan.meta = Some(tagged("missing"));
        transcript.apply(SessionUpdate::AgentMessageChunk(orphan));
        assert!(matches!(&transcript.entries[0], Entry::Agent(text) if text == "lost"));
    }

    #[test]
    fn a_tagged_update_changes_a_call_inside_its_parent() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(ToolCall::new("t1", "Task")));
        let mut child = ToolCall::new("c1", "Read");
        child.meta = Some(tagged("t1"));
        transcript.apply(SessionUpdate::ToolCall(child));
        let mut update = ToolCallUpdate::new("c1", ToolCallUpdateFields::new().title("Read it"));
        update.meta = Some(tagged("t1"));
        transcript.apply(SessionUpdate::ToolCallUpdate(update));
        assert_eq!(transcript.entries.len(), 1);
        let Entry::Tool(task) = &transcript.entries[0] else {
            unreachable!()
        };
        assert!(matches!(&task.children[0], Child::Tool(call) if call.title == "Read it"));
    }
}
