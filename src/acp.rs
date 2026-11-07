use std::path::PathBuf;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOptionKind, PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, StopReason,
    ToolCallUpdate, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{
    Agent, Client, ConnectTo, Error, on_receive_notification, on_receive_request,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub enum Event {
    Update(Box<SessionUpdate>),
    FileRead {
        path: std::path::PathBuf,
        line: Option<u32>,
    },
    Permission {
        update: Box<ToolCallUpdate>,
        choice: String,
    },
    Done(Result<StopReason, Error>),
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
            .terminal(false);
        let request = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(Implementation::new("coax", env!("CARGO_PKG_VERSION")));
        self.cx.send_request(request).block_task().await
    }

    pub async fn new_session(&self, cwd: PathBuf) -> Result<NewSessionResponse, Error> {
        self.cx
            .send_request(NewSessionRequest::new(cwd))
            .block_task()
            .await
    }

    pub fn prompt(&self, session: SessionId, text: String) -> Result<(), Error> {
        let request = PromptRequest::new(session, vec![ContentBlock::from(text)]);
        let sent = self.cx.send_request(request);
        let tx = self.tx.clone();
        self.cx.spawn(async move {
            let result = sent.block_task().await.map(|response| response.stop_reason);
            let _ = tx.send(Event::Done(result));
            Ok(())
        })
    }

    pub fn cancel(&self, session: SessionId) -> Result<(), Error> {
        self.cx.send_notification(CancelNotification::new(session))
    }
}

pub async fn run(
    agent: impl ConnectTo<Client> + 'static,
    app: impl AsyncFnOnce(Link) -> Result<(), Error>,
) -> Result<(), Error> {
    let (tx, events) = mpsc::unbounded_channel();
    let updates = tx.clone();
    let permissions = tx.clone();
    let reads = tx.clone();
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
            async move |req: RequestPermissionRequest, responder, _cx: ConnectionTo<Agent>| {
                let option = req
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    .or(req.options.first());
                let (outcome, choice) = match option {
                    Some(option) => (
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option.option_id.clone(),
                        )),
                        option.name.clone(),
                    ),
                    None => (RequestPermissionOutcome::Cancelled, "cancelled".to_string()),
                };
                let _ = permissions.send(Event::Permission {
                    update: Box::new(req.tool_call),
                    choice,
                });
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ReadTextFileRequest, responder, _cx: ConnectionTo<Agent>| {
                let _ = reads.send(Event::FileRead {
                    path: req.path.clone(),
                    line: req.line,
                });
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
