type Meta = serde_json::Map<String, serde_json::Value>;

use std::time::Instant;

use serde_json::json;

use crate::diff;
use crate::log;
use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, MessageId, PlanEntry, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolKind,
};

pub struct Tool {
    pub id: ToolCallId,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolCallStatus,
    pub locations: Vec<ToolCallLocation>,
    pub content: Vec<ToolCallContent>,

    /// Whether the turn ended or the agent exited before the call finished.
    pub cancelled: bool,

    /// The lines the call's diffs add and remove.
    pub changed: (usize, usize),

    /// How many times `content` has been replaced, so a layout of it can be
    /// reused until it changes.
    pub revision: u32,

    /// Whether the call starts a subagent, though it may have no children
    /// yet, or none at all after a replay.
    pub agent: bool,

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

    /// A message sent to the subagent.
    User(String),
}

#[derive(Clone, Copy)]
enum Role {
    User,
    Agent,
    Thought,
}

pub enum Entry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(Tool),
    Permission { title: String, choice: String },
    Notice(String),
}

/// How many of the agent's last stderr lines an exit notice shows.
const STDERR_LINES: usize = 3;

#[derive(Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    pub plan: Vec<PlanEntry>,
    pub in_flight: bool,
    pub last_stop: Option<StopReason>,

    /// When the latest prompt was sent.
    pub turn_started: Option<Instant>,

    /// The id of the message the last chunk belonged to.
    last_message: Option<MessageId>,
}

impl Transcript {
    pub fn prompt(&mut self, text: String) {
        self.entries.push(Entry::User(text));
        self.in_flight = true;
        self.turn_started = Some(Instant::now());
        self.last_message = None;
    }

    pub fn finished(&mut self, stop: StopReason) {
        self.in_flight = false;
        self.last_stop = Some(stop);
        if stop == StopReason::Cancelled {
            self.cancel_running();
        }
    }

    /// End the turn after the agent's connection ended. `reason` is the
    /// agent's last words: a status on its first line, then what it wrote
    /// to stderr.
    pub fn agent_exited(&mut self, reason: &str) {
        self.in_flight = false;
        self.cancel_running();
        let reason = reason
            .strip_prefix("Process exited with ")
            .unwrap_or(reason);
        let mut lines = reason.lines().filter(|line| !line.trim().is_empty());
        let head = lines.next().unwrap_or_default();
        self.notice(format!("agent exited: {head}"));
        let rest: Vec<&str> = lines.collect();
        for line in &rest[rest.len().saturating_sub(STDERR_LINES)..] {
            self.notice(line.trim().to_string());
        }
    }

    /// Mark every call that has not finished as cancelled.
    pub fn cancel_running(&mut self) {
        fn mark(tool: &mut Tool) {
            if matches!(
                tool.status,
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            ) {
                tool.cancelled = true;
            }
            for child in &mut tool.children {
                if let Child::Tool(child) = child {
                    mark(child);
                }
            }
        }
        for entry in &mut self.entries {
            if let Entry::Tool(tool) = entry {
                mark(tool);
            }
        }
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
        log::info("notice", json!({"text": text}));
        self.entries.push(Entry::Notice(text));
    }

    pub fn apply(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.chunk(Role::Agent, chunk),
            SessionUpdate::AgentThoughtChunk(chunk) => self.chunk(Role::Thought, chunk),
            SessionUpdate::UserMessageChunk(chunk) => self.chunk(Role::User, chunk),
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
                log::info("notice", json!({"text": text}));
                self.entries.push(Entry::Notice(text));
            }
            _ => {}
        }
    }

    /// Add a chunk of text to the message before it, or start a new message
    /// when the chunk is another kind, or carries another message id than the
    /// chunk before it. The chunk goes under its subagent's call when the
    /// adapter tagged it with one.
    fn chunk(&mut self, role: Role, chunk: ContentChunk) {
        let text = content_text(&chunk.content);
        let joins = match (&self.last_message, &chunk.message_id) {
            (Some(last), Some(id)) => last == id,
            _ => true,
        };
        self.last_message = chunk.message_id;
        if let Some(parent) = self.parent(chunk.meta.as_ref()) {
            match (parent.children.last_mut(), role) {
                (Some(Child::Text(last)), Role::Agent)
                | (Some(Child::Thought(last)), Role::Thought)
                | (Some(Child::User(last)), Role::User)
                    if joins =>
                {
                    last.push_str(&text);
                }
                _ => parent.children.push(match role {
                    Role::Agent => Child::Text(text),
                    Role::Thought => Child::Thought(text),
                    Role::User => Child::User(text),
                }),
            }
            return;
        }
        match (self.entries.last_mut(), role) {
            (Some(Entry::Agent(last)), Role::Agent)
            | (Some(Entry::Thought(last)), Role::Thought)
            | (Some(Entry::User(last)), Role::User)
                if joins =>
            {
                last.push_str(&text);
            }
            _ => self.entries.push(match role {
                Role::Agent => Entry::Agent(text),
                Role::Thought => Entry::Thought(text),
                Role::User => Entry::User(text),
            }),
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
            call.meta = meta.clone();
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
            tool.cancelled = false;
        }
        if let Some(locations) = fields.locations {
            tool.locations = locations;
        }
        if let Some(content) = fields.content {
            tool.changed = diff::size(&content);
            tool.content = content;
            tool.revision += 1;
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
        changed: diff::size(&call.content),
        content: call.content,
        cancelled: false,
        revision: 0,
        agent: starts_subagent(call.meta.as_ref()),
        children: Vec::new(),
    }
}

/// Whether the adapter's `_meta.claudeCode.toolName` names the Agent or Task
/// tool, whose calls start subagents.
fn starts_subagent(meta: Option<&Meta>) -> bool {
    let name = meta.and_then(|meta| meta.get("claudeCode")?.get("toolName")?.as_str());
    matches!(name, Some("Agent" | "Task"))
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

    #[test]
    fn a_cancelled_turn_marks_the_calls_still_running_cancelled() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(
            ToolCall::new("t1", "Task").status(ToolCallStatus::InProgress),
        ));
        let mut child = ToolCall::new("c1", "Read").status(ToolCallStatus::Pending);
        child.meta = Some(tagged("t1"));
        transcript.apply(SessionUpdate::ToolCall(child));
        transcript.apply(SessionUpdate::ToolCall(
            ToolCall::new("t2", "Done").status(ToolCallStatus::Completed),
        ));
        transcript.finished(StopReason::Cancelled);
        let Entry::Tool(task) = &transcript.entries[0] else {
            unreachable!()
        };
        assert!(task.cancelled);
        assert!(matches!(&task.children[0], Child::Tool(call) if call.cancelled));
        assert!(matches!(&transcript.entries[1], Entry::Tool(call) if !call.cancelled));
    }

    #[test]
    fn an_exit_notice_gives_the_status_and_the_last_stderr_lines() {
        let mut transcript = Transcript::default();
        transcript.prompt("go".to_string());
        transcript.agent_exited("Process exited with exit status: 1: a\nb\nc\nd\ne");
        assert!(!transcript.in_flight);
        let notices: Vec<&str> = transcript
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Notice(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(notices, ["agent exited: exit status: 1: a", "c", "d", "e"]);
    }

    fn with_id(text: &str, id: &str) -> ContentChunk {
        let mut chunk = chunk(text);
        chunk.message_id = Some(MessageId::new(id));
        chunk
    }

    #[test]
    fn chunks_with_different_message_ids_start_separate_entries() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::UserMessageChunk(with_id("one", "m1")));
        transcript.apply(SessionUpdate::UserMessageChunk(with_id(" more", "m1")));
        transcript.apply(SessionUpdate::UserMessageChunk(with_id("two", "m2")));
        transcript.apply(SessionUpdate::AgentMessageChunk(with_id("a", "m3")));
        transcript.apply(SessionUpdate::AgentMessageChunk(with_id("b", "m4")));
        let texts: Vec<&str> = transcript
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::User(text) | Entry::Agent(text) => text.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(texts, ["one more", "two", "a", "b"]);
    }

    #[test]
    fn chunks_without_a_message_id_still_join() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::UserMessageChunk(with_id("a", "m1")));
        transcript.apply(SessionUpdate::UserMessageChunk(chunk("b")));
        assert_eq!(transcript.entries.len(), 1);
    }

    #[test]
    fn a_tagged_user_message_goes_under_its_subagent_call() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(ToolCall::new("t1", "Task")));
        let mut prompt = chunk("find the bug");
        prompt.meta = Some(tagged("t1"));
        transcript.apply(SessionUpdate::UserMessageChunk(prompt));
        assert_eq!(transcript.entries.len(), 1);
        let Entry::Tool(task) = &transcript.entries[0] else {
            unreachable!()
        };
        assert!(matches!(&task.children[0], Child::User(text) if text == "find the bug"));
    }

    #[test]
    fn the_adapters_tool_name_marks_an_agent_or_task_call_before_it_has_children() {
        let mut meta = Meta::new();
        meta.insert(
            "claudeCode".to_string(),
            serde_json::json!({ "toolName": "Task" }),
        );
        let mut call = ToolCall::new("t1", "Survey").kind(ToolKind::Think);
        call.meta = Some(meta);
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(call));
        transcript.apply(SessionUpdate::ToolCall(
            ToolCall::new("t2", "Plan").kind(ToolKind::Think),
        ));
        assert!(matches!(&transcript.entries[0], Entry::Tool(tool) if tool.agent));
        assert!(matches!(&transcript.entries[1], Entry::Tool(tool) if !tool.agent));
    }

    #[test]
    fn an_edits_size_counts_the_changed_lines_when_the_call_arrives_and_when_it_updates() {
        use agent_client_protocol::v1::schema::Diff;

        let lines = |marker: &str| {
            (0..40)
                .map(|n| {
                    if n == 7 {
                        marker.to_string()
                    } else {
                        format!("line {n}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut diff = Diff::new("a.rs", lines("new"));
        diff.old_text = Some(lines("old"));
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(ToolCall::new("e1", "Edit")));
        transcript.apply(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "e1",
            ToolCallUpdateFields::new().content(vec![ToolCallContent::Diff(diff.clone())]),
        )));
        transcript.apply(SessionUpdate::ToolCall(
            ToolCall::new("e2", "Edit").content(vec![ToolCallContent::Diff(diff)]),
        ));
        for entry in &transcript.entries {
            assert!(matches!(entry, Entry::Tool(tool) if tool.changed == (1, 1)));
        }
    }

    #[test]
    fn a_call_counts_each_time_its_content_is_replaced() {
        let mut transcript = Transcript::default();
        transcript.apply(SessionUpdate::ToolCall(ToolCall::new("c1", "Run")));
        for text in ["one", "two"] {
            transcript.apply(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "c1",
                ToolCallUpdateFields::new().content(vec![ToolCallContent::from(text)]),
            )));
        }
        assert!(matches!(&transcript.entries[0], Entry::Tool(tool) if tool.revision == 2));
    }
}
