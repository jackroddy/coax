use agent_client_protocol::v1::schema::{StopReason, ToolCallStatus, ToolKind};
use agent_client_protocol::{AcpAgent, Error};
use coax::acp::{self, Event, Link};
use coax::fake;
use coax::scenario::Scenario;
use coax::session::{Entry, Transcript};

const HELLO: &str = include_str!("../scenarios/hello.json");

async fn one_prompt(mut link: Link, text: &str) -> Result<Transcript, Error> {
    link.initialize().await?;
    let session = link.new_session(std::env::temp_dir()).await?.session_id;
    let mut transcript = Transcript::default();
    transcript.prompt(text.to_string());
    link.prompt(session, text.to_string())?;
    while let Some(event) = link.events.recv().await {
        match event {
            Event::Update(update) => transcript.apply(*update),
            Event::Permission { update, choice } => transcript.permission(*update, choice),
            Event::FileRead { .. } => {}
            Event::Done(result) => {
                transcript.finished(result?);
                break;
            }
        }
    }
    Ok(transcript)
}

fn check_hello(transcript: &Transcript) {
    assert!(!transcript.in_flight);
    assert_eq!(transcript.last_stop, Some(StopReason::EndTurn));
    let kinds: Vec<&str> = transcript
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::User(_) => "user",
            Entry::Thought(_) => "thought",
            Entry::Tool(_) => "tool",
            Entry::Agent(_) => "agent",
            Entry::Permission { .. } => "permission",
            Entry::Notice(_) => "notice",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "thought",
            "tool",
            "agent",
            "tool",
            "permission",
            "agent"
        ]
    );
    let Entry::Tool(read) = &transcript.entries[2] else {
        unreachable!()
    };
    assert_eq!(read.kind, ToolKind::Read);
    assert_eq!(read.status, ToolCallStatus::Completed);
    assert_eq!(read.locations[0].line, Some(1));
    let Entry::Agent(answer) = &transcript.entries[3] else {
        unreachable!()
    };
    assert!(answer.starts_with("Hello. The entry point is a hello world, "));
    assert!(
        matches!(&transcript.entries[5], Entry::Permission { title, choice } if title == "Write NOTES.md" && choice == "Allow once")
    );
    let Entry::Tool(write) = &transcript.entries[4] else {
        unreachable!()
    };
    assert_eq!(write.title, "Write NOTES.md");
    assert_eq!(write.status, ToolCallStatus::Completed);
    assert_eq!(transcript.plan.len(), 2);
}

#[tokio::test]
async fn the_hello_scenario_builds_the_expected_transcript_in_process() {
    let scenario = Scenario::parse(HELLO).unwrap();
    let transcript = acp::run(fake::agent(scenario), async |link| {
        one_prompt(link, "hello").await.map(|t| {
            check_hello(&t);
        })
    })
    .await;
    transcript.unwrap();
}

#[tokio::test]
async fn the_fake_agent_binary_speaks_the_same_scenario_over_stdio() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios/hello.json");
    let agent = AcpAgent::from_args([env!("CARGO_BIN_EXE_coax-fake-agent"), path]).unwrap();
    acp::run(agent, async |link| {
        let transcript = one_prompt(link, "hello").await?;
        check_hello(&transcript);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_prompt_past_the_last_act_gets_a_message_and_ends_the_turn() {
    let scenario = Scenario::parse(r#"{"acts": []}"#).unwrap();
    acp::run(fake::agent(scenario), async |link| {
        let transcript = one_prompt(link, "anyone there").await?;
        assert!(
            matches!(&transcript.entries[1], Entry::Agent(text) if text.contains("no more acts"))
        );
        assert_eq!(transcript.last_stop, Some(StopReason::EndTurn));
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_cancel_during_an_act_ends_the_turn_with_cancelled() {
    let scenario = Scenario::parse(
        r#"{"acts": [{"steps": [
            {"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "starting"}}},
            {"delay_ms": 200},
            {"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": " and finishing"}}},
            {"stop": "end_turn"}
        ]}]}"#,
    )
    .unwrap();
    acp::run(fake::agent(scenario), async |mut link| {
        link.initialize().await?;
        let session = link.new_session(std::env::temp_dir()).await?.session_id;
        link.prompt(session.clone(), "go".to_string())?;
        let mut transcript = Transcript::default();
        let mut cancelled = false;
        while let Some(event) = link.events.recv().await {
            match event {
                Event::Update(update) => {
                    transcript.apply(*update);
                    if !cancelled {
                        link.cancel(session.clone())?;
                        cancelled = true;
                    }
                }
                Event::Permission { .. } | Event::FileRead { .. } => {}
                Event::Done(result) => {
                    transcript.finished(result?);
                    break;
                }
            }
        }
        assert_eq!(transcript.last_stop, Some(StopReason::Cancelled));
        assert!(matches!(&transcript.entries[0], Entry::Agent(text) if text == "starting"));
        Ok(())
    })
    .await
    .unwrap();
}
