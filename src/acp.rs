use std::path::PathBuf;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, InitializeResponse, ListSessionsRequest, LoadSessionRequest,
    LoadSessionResponse, McpServer, McpServerHttp, NewSessionRequest, NewSessionResponse,
    PermissionOption, PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionConfigOptionValue, SessionId, SessionInfo, SessionModeId, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest, StopReason,
    ToolCallUpdate, Usage, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{
    Agent, Client, ConnectTo, Error, on_receive_notification, on_receive_request,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

pub enum Event {
    Update(Box<SessionUpdate>),
    Permission {
        update: Box<ToolCallUpdate>,
        options: Vec<PermissionOption>,
        reply: oneshot::Sender<RequestPermissionOutcome>,
    },
    Tokens(Usage),
    Setting(Result<Applied, Error>),
    Done(Result<StopReason, Error>),
}

/// A setting to change.
pub enum Change {
    Config {
        id: String,
        value: SessionConfigOptionValue,
    },
    Mode(String),
}

/// A setting change the agent accepted.
pub enum Applied {
    Options(Vec<agent_client_protocol::v1::schema::SessionConfigOption>),
    Mode(String),
}

pub struct Link {
    pub cx: ConnectionTo<Agent>,
    pub events: UnboundedReceiver<Event>,
    tx: UnboundedSender<Event>,
}

impl Link {
    pub async fn initialize(&self) -> Result<InitializeResponse, Error> {
        let capabilities = ClientCapabilities::new()
            .fs(FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true))
            .terminal(false)
            // the Claude adapter sends a subagent's text only to a client
            // that declares this
            .meta(serde_json::Map::from_iter([(
                "subagent-transcript".to_string(),
                serde_json::Value::Bool(true),
            )]));
        let request = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(Implementation::new("coax", env!("CARGO_PKG_VERSION")));
        self.cx.send_request(request).block_task().await
    }

    pub async fn new_session(&self, cwd: PathBuf) -> Result<NewSessionResponse, Error> {
        let request = NewSessionRequest::new(cwd).mcp_servers(mcp_servers());
        self.cx.send_request(request).block_task().await
    }

    /// The sessions the agent has for `cwd`, following `nextCursor` to the end.
    pub async fn list_sessions(&self, cwd: PathBuf) -> Result<Vec<SessionInfo>, Error> {
        let mut sessions = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let request = ListSessionsRequest::new()
                .cwd(cwd.clone())
                .cursor(cursor.clone());
            let page = self.cx.send_request(request).block_task().await?;
            sessions.extend(page.sessions);
            match page.next_cursor {
                // an agent that repeats a cursor would loop here for good
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                _ => return Ok(sessions),
            }
        }
    }

    /// Load `session`. The agent replays its history as updates on `events`
    /// before this returns.
    pub async fn load_session(
        &self,
        session: SessionId,
        cwd: PathBuf,
    ) -> Result<LoadSessionResponse, Error> {
        let request = LoadSessionRequest::new(session, cwd).mcp_servers(mcp_servers());
        self.cx.send_request(request).block_task().await
    }

    pub fn prompt(&self, session: SessionId, text: String) -> Result<(), Error> {
        let request = PromptRequest::new(session, vec![ContentBlock::from(text)]);
        let sent = self.cx.send_request(request);
        let tx = self.tx.clone();
        self.cx.spawn(async move {
            let response = sent.block_task().await;
            if let Ok(response) = &response
                && let Some(usage) = &response.usage
            {
                let _ = tx.send(Event::Tokens(usage.clone()));
            }
            let _ = tx.send(Event::Done(response.map(|response| response.stop_reason)));
            Ok(())
        })
    }

    /// Ask the agent to change a setting. The answer arrives as an
    /// `Event::Setting`.
    pub fn change(&self, session: SessionId, change: Change) -> Result<(), Error> {
        let tx = self.tx.clone();
        match change {
            Change::Config { id, value } => {
                let sent = self
                    .cx
                    .send_request(SetSessionConfigOptionRequest::new(session, id, value));
                self.cx.spawn(async move {
                    let applied = sent
                        .block_task()
                        .await
                        .map(|response| Applied::Options(response.config_options));
                    let _ = tx.send(Event::Setting(applied));
                    Ok(())
                })
            }
            Change::Mode(id) => {
                let sent = self.cx.send_request(SetSessionModeRequest::new(
                    session,
                    SessionModeId::new(id.clone()),
                ));
                self.cx.spawn(async move {
                    let applied = sent.block_task().await.map(|_| Applied::Mode(id));
                    let _ = tx.send(Event::Setting(applied));
                    Ok(())
                })
            }
        }
    }

    pub fn cancel(&self, session: SessionId) -> Result<(), Error> {
        self.cx.send_notification(CancelNotification::new(session))
    }
}

fn mcp_servers() -> Vec<McpServer> {
    match std::env::var("DECADE_MCP") {
        Ok(url) => vec![McpServer::Http(McpServerHttp::new("decade", url))],
        Err(_) => Vec::new(),
    }
}

pub async fn run(
    agent: impl ConnectTo<Client> + 'static,
    app: impl AsyncFnOnce(Link) -> Result<(), Error>,
) -> Result<(), Error> {
    let (tx, events) = mpsc::unbounded_channel();
    let updates = tx.clone();
    let permissions = tx.clone();
    Client
        .v1()
        .name("coax")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                let _ = updates.send(Event::Update(Box::new(notification.update)));
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, cx: ConnectionTo<Agent>| {
                let (reply, answer) = oneshot::channel();
                let _ = permissions.send(Event::Permission {
                    update: Box::new(req.tool_call),
                    options: req.options,
                    reply,
                });
                cx.spawn(async move {
                    let outcome = answer.await.unwrap_or(RequestPermissionOutcome::Cancelled);
                    responder.respond(RequestPermissionResponse::new(outcome))
                })
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ReadTextFileRequest, responder, _cx: ConnectionTo<Agent>| {
                match tokio::fs::read_to_string(&req.path).await {
                    Ok(text) => responder.respond(ReadTextFileResponse::new(text)),
                    Err(error) => responder.respond_with_internal_error(error),
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async |req: WriteTextFileRequest, responder, _cx: ConnectionTo<Agent>| {
                match tokio::fs::write(&req.path, req.content).await {
                    Ok(()) => responder.respond(WriteTextFileResponse::new()),
                    Err(error) => responder.respond_with_internal_error(error),
                }
            },
            on_receive_request!(),
        )
        .connect_with(agent, async |cx: ConnectionTo<Agent>| {
            app(Link { cx, events, tx }).await
        })
        .await
}
