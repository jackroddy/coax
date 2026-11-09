use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionCapabilities,
    SessionConfigKind, SessionConfigOptionValue, SessionConfigSelectOptions, SessionId,
    SessionInfo, SessionListCapabilities, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    SetSessionModeResponse, StopReason,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{
    Agent, Client, ConnectTo, Error, on_receive_notification, on_receive_request,
};

use crate::scenario::{Act, Scenario, Step};

pub fn agent(scenario: Scenario) -> impl ConnectTo<Client> {
    let options = Arc::new(Mutex::new(scenario.config_options));
    let modes = Arc::new(Mutex::new(scenario.modes));
    let (new_options, load_options, set_options) =
        (options.clone(), options.clone(), options.clone());
    let (new_modes, load_modes, set_modes) = (modes.clone(), modes.clone(), modes.clone());
    let saved = Arc::new(scenario.sessions);
    let advertised = saved.clone();
    let listed = saved.clone();
    let acts = Arc::new(Mutex::new(VecDeque::from(scenario.acts)));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_flag = cancelled.clone();
    Agent
        .v1()
        .name("coax-fake-agent")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _cx: ConnectionTo<Client>| {
                let mut capabilities = AgentCapabilities::new();
                if !advertised.is_empty() {
                    capabilities = capabilities.load_session(true).session_capabilities(
                        SessionCapabilities::new().list(SessionListCapabilities::new()),
                    );
                }
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(capabilities)
                        .agent_info(Implementation::new(
                            "coax-fake-agent",
                            env!("CARGO_PKG_VERSION"),
                        )),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _cx: ConnectionTo<Client>| {
                let options = new_options.lock().unwrap().clone();
                responder.respond(
                    NewSessionResponse::new("fake-session")
                        .config_options((!options.is_empty()).then_some(options))
                        .modes(new_modes.lock().unwrap().clone()),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionConfigOptionRequest,
                        responder,
                        _cx: ConnectionTo<Client>| {
                let mut options = set_options.lock().unwrap();
                let Some(option) = options.iter_mut().find(|option| option.id == req.config_id)
                else {
                    return responder.respond_with_error(Error::invalid_params());
                };
                let accepted = match (&mut option.kind, &req.value) {
                    (
                        SessionConfigKind::Select(select),
                        SessionConfigOptionValue::ValueId { value },
                    ) => {
                        let known = match &select.options {
                            SessionConfigSelectOptions::Ungrouped(choices) => {
                                choices.iter().any(|choice| choice.value == *value)
                            }
                            SessionConfigSelectOptions::Grouped(groups) => groups
                                .iter()
                                .flat_map(|group| &group.options)
                                .any(|choice| choice.value == *value),
                            _ => false,
                        };
                        if known {
                            select.current_value = value.clone();
                        }
                        known
                    }
                    (
                        SessionConfigKind::Boolean(boolean),
                        SessionConfigOptionValue::Boolean { value },
                    ) => {
                        boolean.current_value = *value;
                        true
                    }
                    _ => false,
                };
                if accepted {
                    responder.respond(SetSessionConfigOptionResponse::new(options.clone()))
                } else {
                    responder.respond_with_error(Error::invalid_params())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest, responder, _cx: ConnectionTo<Client>| {
                let mut modes = set_modes.lock().unwrap();
                match modes.as_mut() {
                    Some(state)
                        if state
                            .available_modes
                            .iter()
                            .any(|mode| mode.id == req.mode_id) =>
                    {
                        state.current_mode_id = req.mode_id;
                        responder.respond(SetSessionModeResponse::new())
                    }
                    _ => responder.respond_with_error(Error::invalid_params()),
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ListSessionsRequest, responder, _cx: ConnectionTo<Client>| {
                let cwd = req.cwd.unwrap_or_default();
                let sessions = listed
                    .iter()
                    .map(|saved| {
                        SessionInfo::new(saved.id.clone(), cwd.clone())
                            .title(saved.title.clone())
                            .updated_at(saved.updated_at.clone())
                    })
                    .collect();
                responder.respond(ListSessionsResponse::new(sessions))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                let Some(saved) = saved.iter().find(|saved| *saved.id == *req.session_id.0) else {
                    return responder.respond_with_error(Error::invalid_params());
                };
                for step in &saved.history {
                    if let Step::Update(update) = step {
                        cx.send_notification(SessionNotification::new(
                            req.session_id.clone(),
                            update.clone(),
                        ))?;
                    }
                }
                let options = load_options.lock().unwrap().clone();
                responder.respond(
                    LoadSessionResponse::new()
                        .config_options((!options.is_empty()).then_some(options))
                        .modes(load_modes.lock().unwrap().clone()),
                )
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, cx: ConnectionTo<Client>| {
                let act = acts.lock().unwrap().pop_front();
                let session = req.session_id;
                let player = cx.clone();
                let cancelled = cancelled.clone();
                cancelled.store(false, Ordering::SeqCst);
                cx.spawn(async move { play(player, session, act, cancelled, responder).await })?;
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async move |_n: CancelNotification, _cx: ConnectionTo<Client>| {
                cancel_flag.store(true, Ordering::SeqCst);
                Ok(())
            },
            on_receive_notification!(),
        )
}

async fn play(
    cx: ConnectionTo<Client>,
    session: SessionId,
    act: Option<Act>,
    cancelled: Arc<AtomicBool>,
    responder: Responder<PromptResponse>,
) -> Result<(), Error> {
    let usage = act.as_ref().and_then(|act| act.usage.clone());
    let Some(act) = act else {
        let text = ContentBlock::from("the scenario has no more acts");
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(text));
        cx.send_notification(SessionNotification::new(session, update))?;
        return responder.respond(PromptResponse::new(StopReason::EndTurn));
    };
    for step in act.steps {
        if cancelled.load(Ordering::SeqCst) {
            return responder.respond(PromptResponse::new(StopReason::Cancelled));
        }
        match step {
            Step::Update(update) => {
                cx.send_notification(SessionNotification::new(session.clone(), update))?;
            }
            Step::DelayMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            Step::Permission(permission) => {
                let request = RequestPermissionRequest::new(
                    session.clone(),
                    permission.tool_call,
                    permission.options,
                );
                let answer = cx.send_request(request).block_task().await?;
                if let Some(expect) = permission.expect {
                    let chosen = match &answer.outcome {
                        RequestPermissionOutcome::Selected(selected) => {
                            selected.option_id.to_string()
                        }
                        _ => "cancelled".to_string(),
                    };
                    if chosen != expect {
                        eprintln!("coax-fake-agent: expected permission {expect}, got {chosen}");
                    }
                }
            }
            Step::Stop(reason) => {
                return responder.respond(PromptResponse::new(reason).usage(usage));
            }
        }
    }
    responder.respond(PromptResponse::new(StopReason::EndTurn).usage(usage))
}
