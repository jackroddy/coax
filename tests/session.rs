use agent_client_protocol::v1::schema::{
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome, StopReason,
    ToolCallStatus, ToolKind,
};
use agent_client_protocol::{AcpAgent, Error};
use coax::acp::{self, Applied, Change, Event, Link};
use coax::fake;
use coax::scenario::Scenario;
use coax::session::{Child, Entry, Transcript};
use coax::settings::Settings;

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
            Event::Permission {
                update,
                options,
                reply,
            } => {
                let allow = options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    .expect("the scenario offers an allow option");
                reply
                    .send(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(allow.option_id.clone()),
                    ))
                    .unwrap();
                transcript.permission(*update, allow.name.clone());
            }
            Event::Tokens(_) | Event::Setting(_) | Event::Exited(_) => {}
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
    let transcript = acp::run(fake::agent(scenario), "fake", async |link| {
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
    acp::run(agent, "fake", async |link| {
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
    acp::run(fake::agent(scenario), "fake", async |link| {
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
    acp::run(fake::agent(scenario), "fake", async |mut link| {
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
                Event::Permission { .. } => {}
                Event::Tokens(_) | Event::Setting(_) | Event::Exited(_) => {}
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

/// The next setting event, past the updates a new session sends.
async fn next_setting(link: &mut Link) -> Option<Event> {
    while let Some(event) = link.events.recv().await {
        if matches!(event, Event::Setting(_)) {
            return Some(event);
        }
    }
    None
}

const RESUME: &str = include_str!("../scenarios/resume.json");

#[tokio::test]
async fn the_fake_agent_lists_the_sessions_in_its_scenario() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |link| {
        let init = link.initialize().await?;
        assert!(init.agent_capabilities.load_session);
        assert!(init.agent_capabilities.session_capabilities.list.is_some());
        let sessions = link.list_sessions(std::env::temp_dir()).await?;
        let titles: Vec<_> = sessions
            .iter()
            .map(|info| info.title.as_deref().unwrap())
            .collect();
        assert_eq!(titles, ["Read the entry point", "Plan the notes file"]);
        assert_eq!(&*sessions[0].session_id.0, "earlier-1");
        assert_eq!(
            sessions[1].updated_at.as_deref(),
            Some("2026-11-09T06:45:00Z")
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_scenario_with_no_sessions_advertises_neither_list_nor_load() {
    let scenario = Scenario::parse(HELLO).unwrap();
    acp::run(fake::agent(scenario), "fake", async |link| {
        let init = link.initialize().await?;
        assert!(!init.agent_capabilities.load_session);
        assert!(init.agent_capabilities.session_capabilities.list.is_none());
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn loading_a_session_replays_its_history_into_the_transcript() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios/resume.json");
    let agent = AcpAgent::from_args([env!("CARGO_BIN_EXE_coax-fake-agent"), path]).unwrap();
    acp::run(agent, "fake", async |mut link| {
        link.initialize().await?;
        link.load_session("earlier-1".into(), std::env::temp_dir())
            .await?;
        let mut transcript = Transcript::default();
        while let Ok(Event::Update(update)) = link.events.try_recv() {
            transcript.apply(*update);
        }
        assert!(matches!(&transcript.entries[0], Entry::User(text) if text == "What does src/main.rs do?"));
        assert!(matches!(&transcript.entries[1], Entry::Tool(tool) if tool.title == "Read src/main.rs"));
        assert!(matches!(
            &transcript.entries[2],
            Entry::Agent(text) if text == "It is a hello world. Nothing else lives there yet."
        ));
        assert_eq!(transcript.entries.len(), 3);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn loading_an_unknown_session_is_an_error() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |link| {
        link.initialize().await?;
        let loaded = link
            .load_session("missing".into(), std::env::temp_dir())
            .await;
        assert!(loaded.is_err());
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_prompt_after_loading_plays_the_scenarios_acts() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |mut link| {
        link.initialize().await?;
        link.load_session("earlier-2".into(), std::env::temp_dir())
            .await?;
        while link.events.try_recv().is_ok() {}
        link.prompt("earlier-2".into(), "hello".to_string())?;
        let mut transcript = Transcript::default();
        while let Some(event) = link.events.recv().await {
            match event {
                Event::Update(update) => transcript.apply(*update),
                Event::Permission { options, reply, .. } => {
                    reply
                        .send(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(options[0].option_id.clone()),
                        ))
                        .unwrap();
                }
                Event::Tokens(_) | Event::Setting(_) | Event::Exited(_) => {}
                Event::Done(result) => {
                    transcript.finished(result?);
                    break;
                }
            }
        }
        assert!(matches!(&transcript.entries[0], Entry::Thought(_)));
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_new_session_offers_the_scenarios_settings_in_header_order() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |link| {
        link.initialize().await?;
        let response = link.new_session(std::env::temp_dir()).await?;
        let mut settings = Settings::default();
        settings.open(response.config_options, response.modes);
        let labels: Vec<String> = settings.settings().into_iter().map(|s| s.label).collect();
        assert_eq!(labels, ["Auto", "Opus", "Effort Default", "Fast off"]);
        assert!(settings.settings()[0].unguarded);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn changing_a_setting_returns_the_updated_set_and_a_bad_value_is_an_error() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |mut link| {
        link.initialize().await?;
        let session = link.new_session(std::env::temp_dir()).await?.session_id;
        let choose = |id: &str, value: &str| Change::Config {
            id: id.to_string(),
            value: value.into(),
        };
        link.change(session.clone(), choose("model", "sonnet"))?;
        let Some(Event::Setting(Ok(Applied::Options(options)))) = next_setting(&mut link).await
        else {
            panic!("expected the updated options");
        };
        let mut settings = Settings::default();
        settings.set_options(options);
        assert_eq!(settings.settings()[1].label, "Sonnet");

        link.change(session.clone(), choose("model", "gpt"))?;
        assert!(matches!(
            next_setting(&mut link).await,
            Some(Event::Setting(Err(_)))
        ));

        link.change(
            session,
            Change::Config {
                id: "fast".to_string(),
                value: true.into(),
            },
        )?;
        let Some(Event::Setting(Ok(Applied::Options(options)))) = next_setting(&mut link).await
        else {
            panic!("expected the updated options");
        };
        settings.set_options(options);
        assert_eq!(settings.settings()[3].label, "Fast on");
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_turn_reports_the_context_the_cost_and_the_session_tokens() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |mut link| {
        link.initialize().await?;
        let session = link.new_session(std::env::temp_dir()).await?.session_id;
        link.prompt(session, "hello".to_string())?;
        let mut settings = Settings::default();
        while let Some(event) = link.events.recv().await {
            match event {
                Event::Update(update) => settings.apply(&update),
                Event::Tokens(usage) => settings.used(&usage),
                Event::Permission { options, reply, .. } => {
                    reply
                        .send(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(options[0].option_id.clone()),
                        ))
                        .unwrap();
                }
                Event::Done(_) => break,
                Event::Setting(_) | Event::Exited(_) => {}
            }
        }
        let context = settings.context.unwrap();
        assert_eq!((context.used, context.size), (28481, 1_000_000));
        assert_eq!(settings.cost.unwrap().amount, 0.136);
        let tokens = settings.tokens.unwrap();
        assert_eq!((tokens.up, tokens.down), (28481, 4));
        Ok(())
    })
    .await
    .unwrap();
}

const SUBAGENTS: &str = include_str!("../scenarios/subagents.json");

async fn subagent_transcript() -> Transcript {
    let scenario = Scenario::parse(SUBAGENTS).unwrap();
    let saved = std::sync::Arc::new(std::sync::Mutex::new(None));
    let keep = saved.clone();
    acp::run(fake::agent(scenario), "fake", async move |link| {
        *keep.lock().unwrap() = Some(one_prompt(link, "go").await?);
        Ok(())
    })
    .await
    .unwrap();
    let taken = saved.lock().unwrap().take();
    taken.unwrap()
}

#[tokio::test]
async fn a_subagents_calls_and_text_group_under_its_task_call() {
    let transcript = subagent_transcript().await;
    let kinds: Vec<&str> = transcript
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::User(_) => "user",
            Entry::Agent(_) => "agent",
            Entry::Tool(_) => "tool",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "agent", "tool", "tool", "agent"]);
    let Entry::Tool(survey) = &transcript.entries[2] else {
        unreachable!()
    };
    assert_eq!(survey.title, "Survey the entry point");
    assert_eq!(survey.status, ToolCallStatus::Completed);
    assert_eq!(survey.tool_count(), 1);
    assert!(
        matches!(&survey.children[0], Child::Tool(call) if call.title == "Read src/main.rs" && call.status == ToolCallStatus::Completed)
    );
    assert!(matches!(&survey.children[1], Child::Text(text) if text == "main.rs is a stub."));
    let Entry::Tool(config) = &transcript.entries[3] else {
        unreachable!()
    };
    assert_eq!(config.children.len(), 2);
    assert!(
        matches!(&config.children[1], Child::Text(text) if text == "Cargo.toml names the crate.")
    );
}

#[tokio::test]
async fn text_never_merges_across_a_subagent_boundary() {
    let transcript = subagent_transcript().await;
    assert!(
        matches!(&transcript.entries[1], Entry::Agent(text) if text == "Spawning two helpers. ")
    );
    assert!(
        matches!(&transcript.entries[4], Entry::Agent(text) if text == "Both helpers are done.")
    );
}

#[tokio::test]
async fn a_new_session_advertises_the_scenarios_commands() {
    let scenario = Scenario::parse(RESUME).unwrap();
    acp::run(fake::agent(scenario), "fake", async |mut link| {
        link.initialize().await?;
        link.new_session(std::env::temp_dir()).await?;
        let Some(Event::Update(update)) = link.events.recv().await else {
            panic!("expected the commands");
        };
        let mut settings = Settings::default();
        settings.apply(&update);
        let names: Vec<&str> = settings.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["compact", "review", "init"]);
        Ok(())
    })
    .await
    .unwrap();
}

const BLOCKS: &str = include_str!("../scenarios/blocks.json");

#[tokio::test]
async fn an_edits_diff_and_a_runs_output_arrive_in_the_calls_content() {
    use agent_client_protocol::v1::schema::{ContentBlock, ToolCallContent};

    let scenario = Scenario::parse(BLOCKS).unwrap();
    let saved = std::sync::Arc::new(std::sync::Mutex::new(None));
    let keep = saved.clone();
    acp::run(fake::agent(scenario), "fake", async move |link| {
        *keep.lock().unwrap() = Some(one_prompt(link, "go").await?);
        Ok(())
    })
    .await
    .unwrap();
    let transcript = saved.lock().unwrap().take().unwrap();
    let Entry::Agent(text) = &transcript.entries[1] else {
        panic!("expected the agent's text first");
    };
    assert!(text.contains("```rust"));
    let Entry::Tool(edit) = &transcript.entries[2] else {
        panic!("expected the edit");
    };
    assert_eq!(edit.kind, ToolKind::Edit);
    let [ToolCallContent::Diff(diff)] = &edit.content[..] else {
        panic!("expected one diff");
    };
    assert!(
        diff.old_text
            .as_deref()
            .unwrap()
            .contains("println!(\"hello\")")
    );
    assert!(diff.new_text.contains("run();"));
    let Entry::Tool(run) = &transcript.entries[3] else {
        panic!("expected the run");
    };
    assert_eq!(run.kind, ToolKind::Execute);
    let [ToolCallContent::Content(content)] = &run.content[..] else {
        panic!("expected one piece of output");
    };
    assert!(
        matches!(&content.content, ContentBlock::Text(text) if text.text.starts_with("```console"))
    );
}

#[tokio::test]
async fn an_agent_that_exits_mid_turn_ends_the_link_with_its_status_and_stderr() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios/exits.json");
    let agent = AcpAgent::from_args([env!("CARGO_BIN_EXE_coax-fake-agent"), path]).unwrap();
    let mut reason = String::new();
    acp::run(agent, "fake", async |mut link| {
        link.initialize().await?;
        let session = link.new_session(std::env::temp_dir()).await?.session_id;
        link.prompt(session, "go".to_string())?;
        while let Some(event) = link.events.recv().await {
            match event {
                Event::Exited(text) => {
                    reason = text;
                    return Ok(());
                }
                Event::Done(_) => panic!("a dead agent answered the prompt"),
                _ => {}
            }
        }
        panic!("the events ended without an exit");
    })
    .await
    .unwrap();
    assert!(reason.contains("exit status: 3"), "{reason}");
    assert!(reason.contains("second line"), "{reason}");
}

#[tokio::test]
async fn a_command_that_does_not_exist_is_named_in_the_error() {
    let agent = AcpAgent::from_args(["coax-no-such-agent"]).unwrap();
    let error = acp::run(agent, "coax-no-such-agent", async |link| {
        link.initialize().await?;
        Ok(())
    })
    .await
    .unwrap_err();
    let text = acp::describe(&error);
    assert!(text.contains("coax-no-such-agent"), "{text}");
    assert!(text.contains("No such file"), "{text}");
}
