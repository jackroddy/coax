use std::io;
use std::path::Path;

use agent_client_protocol::v1::schema::{StopReason, ToolCallStatus};
use decade_channel::{Agent, AgentState, AgentUpdate, Channel, Context, Message, Tokens};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::session::{Entry, Transcript};
use crate::settings::Settings;

/// What coax tells decade about its agent at one moment.
#[derive(Clone, Default, PartialEq)]
pub struct Snapshot {
    pub state: Option<AgentState>,
    pub activity: Option<String>,
    pub question: Option<String>,
    pub model: Option<String>,
    pub context: Option<Context>,
    pub tokens: Option<Tokens>,
    pub cost: Option<f64>,
}

impl Snapshot {
    /// `question` is the title of the permission request waiting for an
    /// answer, if one is.
    pub fn of(transcript: &Transcript, question: Option<&str>, settings: &Settings) -> Snapshot {
        let state = if question.is_some() {
            AgentState::Attention
        } else if transcript.in_flight {
            AgentState::Working
        } else {
            match transcript.last_stop {
                None | Some(StopReason::Cancelled) => AgentState::Idle,
                Some(_) => AgentState::Done,
            }
        };
        Snapshot {
            state: Some(state),
            activity: transcript
                .in_flight
                .then(|| running_title(transcript))
                .flatten(),
            question: question.map(str::to_string),
            model: settings.model(),
            context: settings.context.as_ref().map(|context| Context {
                used: context.used,
                limit: context.size,
            }),
            tokens: settings.tokens.as_ref().map(|tokens| Tokens {
                input: tokens.up,
                output: tokens.down,
            }),
            cost: settings
                .cost
                .as_ref()
                .filter(|cost| cost.currency == "USD")
                .map(|cost| cost.amount),
        }
    }
}

/// The title of the latest call of this turn that has not finished.
fn running_title(transcript: &Transcript) -> Option<String> {
    transcript
        .entries
        .iter()
        .rev()
        .take_while(|entry| !matches!(entry, Entry::User(_)))
        .find_map(|entry| match entry {
            Entry::Tool(tool)
                if matches!(
                    tool.status,
                    ToolCallStatus::Pending | ToolCallStatus::InProgress
                ) && !tool.cancelled =>
            {
                Some(tool.title.clone())
            }
            _ => None,
        })
}

/// The fields of `now` that differ from `sent`, with a field that went away
/// cleared. `None` when nothing differs.
pub fn changes(sent: &Snapshot, now: &Snapshot) -> Option<AgentUpdate> {
    fn field<T: Clone + PartialEq>(sent: &Option<T>, now: &Option<T>) -> Option<Option<T>> {
        (sent != now).then(|| now.clone())
    }
    let update = AgentUpdate {
        state: field(&sent.state, &now.state),
        activity: field(&sent.activity, &now.activity),
        question: field(&sent.question, &now.question),
        model: field(&sent.model, &now.model),
        context: field(&sent.context, &now.context),
        tokens: field(&sent.tokens, &now.tokens),
        cost: field(&sent.cost, &now.cost),
    };
    (update != AgentUpdate::default()).then_some(update)
}

/// The decade channel, and what it has been told of the agent so far.
pub struct Reporter {
    channel: Channel,
    sent: Snapshot,
}

/// Join decade at `path` as coax running `agent`. The messages decade sends
/// arrive on the receiver.
pub fn connect(
    path: &Path,
    pane: Option<&str>,
    agent: &Agent,
) -> io::Result<(Reporter, UnboundedReceiver<Message>)> {
    let (channel, messages) = Channel::connect(path, "coax", &[], &[], pane, Some(agent))?;
    Ok(reporter(channel, messages))
}

/// Join decade by the environment's socket, if there is one.
pub fn connect_env(agent: &Agent) -> Option<(Reporter, UnboundedReceiver<Message>)> {
    let joined = Channel::from_env("coax", &[], &[], Some(agent))?;
    let (channel, messages) = joined.ok()?;
    Some(reporter(channel, messages))
}

fn reporter(
    channel: Channel,
    messages: std::sync::mpsc::Receiver<Message>,
) -> (Reporter, UnboundedReceiver<Message>) {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for message in messages {
            if tx.send(message).is_err() {
                return;
            }
        }
    });
    (
        Reporter {
            channel,
            sent: Snapshot::default(),
        },
        rx,
    )
}

impl Reporter {
    /// Send what changed since the last call.
    pub fn send(&mut self, now: Snapshot) -> io::Result<()> {
        if let Some(update) = changes(&self.sent, &now) {
            self.channel.agent(&update)?;
        }
        self.sent = now;
        Ok(())
    }

    pub fn end(mut self) {
        let _ = self.channel.ended();
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{Cost, SessionUpdate, ToolCall, UsageUpdate};

    use super::*;

    fn snapshot(transcript: &Transcript, question: Option<&str>) -> Snapshot {
        Snapshot::of(transcript, question, &Settings::default())
    }

    fn state_of(update: Option<AgentUpdate>) -> Option<Option<AgentState>> {
        update.unwrap().state
    }

    #[test]
    fn a_fresh_session_reports_idle_once_and_then_nothing() {
        let transcript = Transcript::default();
        let now = snapshot(&transcript, None);
        let first = changes(&Snapshot::default(), &now).unwrap();
        assert_eq!(first.state, Some(Some(AgentState::Idle)));
        assert_eq!(first.activity, None);
        assert_eq!(changes(&now, &now), None);
    }

    #[test]
    fn a_prompt_turns_the_state_to_working_and_a_normal_end_to_done() {
        let mut transcript = Transcript::default();
        let idle = snapshot(&transcript, None);
        transcript.prompt("hi".to_string());
        let working = snapshot(&transcript, None);
        assert_eq!(
            state_of(changes(&idle, &working)),
            Some(Some(AgentState::Working))
        );
        transcript.finished(StopReason::EndTurn);
        let done = snapshot(&transcript, None);
        assert_eq!(
            state_of(changes(&working, &done)),
            Some(Some(AgentState::Done))
        );
    }

    #[test]
    fn a_cancelled_turn_ends_idle() {
        let mut transcript = Transcript::default();
        transcript.prompt("hi".to_string());
        let working = snapshot(&transcript, None);
        transcript.finished(StopReason::Cancelled);
        let idle = snapshot(&transcript, None);
        assert_eq!(
            state_of(changes(&working, &idle)),
            Some(Some(AgentState::Idle))
        );
    }

    #[test]
    fn a_waiting_permission_asks_with_the_tool_title_and_working_resumes_when_it_clears() {
        let mut transcript = Transcript::default();
        transcript.prompt("hi".to_string());
        let working = snapshot(&transcript, None);
        let asking = snapshot(&transcript, Some("Write NOTES.md"));
        let update = changes(&working, &asking).unwrap();
        assert_eq!(update.state, Some(Some(AgentState::Attention)));
        assert_eq!(update.question, Some(Some("Write NOTES.md".to_string())));
        let update = changes(&asking, &working).unwrap();
        assert_eq!(update.state, Some(Some(AgentState::Working)));
        assert_eq!(update.question, Some(None));
    }

    #[test]
    fn the_activity_is_the_running_calls_title_and_clears_when_it_finishes() {
        let mut transcript = Transcript::default();
        transcript.prompt("hi".to_string());
        let before = snapshot(&transcript, None);
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "toolCallId": "c1", "title": "Read src/main.rs", "status": "in_progress"
        }))
        .unwrap();
        transcript.apply(SessionUpdate::ToolCall(call));
        let running = snapshot(&transcript, None);
        let update = changes(&before, &running).unwrap();
        assert_eq!(update.activity, Some(Some("Read src/main.rs".to_string())));
        assert_eq!(update.state, None);
        let update: agent_client_protocol::v1::schema::ToolCallUpdate =
            serde_json::from_value(serde_json::json!({
                "toolCallId": "c1", "status": "completed"
            }))
            .unwrap();
        transcript.apply(SessionUpdate::ToolCallUpdate(update));
        let after = snapshot(&transcript, None);
        assert_eq!(changes(&running, &after).unwrap().activity, Some(None));
    }

    #[test]
    fn model_context_tokens_and_cost_come_from_the_settings() {
        let transcript = Transcript::default();
        let mut settings = Settings::default();
        settings.apply(&SessionUpdate::UsageUpdate(
            UsageUpdate::new(81_000, 200_000).cost(Cost::new(1.5, "USD")),
        ));
        let now = Snapshot::of(&transcript, None, &settings);
        assert_eq!(
            now.context,
            Some(Context {
                used: 81_000,
                limit: 200_000
            })
        );
        assert_eq!(now.cost, Some(1.5));
        let update = changes(&snapshot(&transcript, None), &now).unwrap();
        assert_eq!(update.cost, Some(Some(1.5)));
        assert_eq!(update.model, None);
    }

    #[test]
    fn a_cost_in_another_currency_is_not_reported() {
        let mut settings = Settings::default();
        settings.apply(&SessionUpdate::UsageUpdate(
            UsageUpdate::new(1, 2).cost(Cost::new(2.0, "EUR")),
        ));
        let now = Snapshot::of(&Transcript::default(), None, &settings);
        assert_eq!(now.cost, None);
    }
}
