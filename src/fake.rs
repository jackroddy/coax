use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionCapabilities,
    SessionId, SessionInfo, SessionListCapabilities, SessionNotification, SessionUpdate,
    StopReason,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{
    Agent, Client, ConnectTo, Error, on_receive_notification, on_receive_request,
};

use crate::scenario::{Act, Scenario, Step};

pub fn agent(scenario: Scenario) -> impl ConnectTo<Client> {
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
            async |_req: NewSessionRequest, responder, _cx: ConnectionTo<Client>| {
                responder.respond(NewSessionResponse::new("fake-session"))
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
                responder.respond(LoadSessionResponse::new())
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
            Step::Stop(reason) => return responder.respond(PromptResponse::new(reason)),
        }
    }
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}
