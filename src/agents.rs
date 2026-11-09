use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{InitializeResponse, SessionInfo};
use agent_client_protocol::{AcpAgent, Error};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, spawn_local};

use crate::acp::{self, Link};
use crate::app::{self, Opening};
use crate::config::Config;

/// The sessions one agent reported for the folder, or none when it failed
/// to start, to initialize or to list.
pub struct Listing {
    pub agent: String,
    pub sessions: Vec<SessionInfo>,
}

/// Every configured agent, started at once. Each runs on a local task that
/// lists its sessions, then waits for the picker to hand it the opening or
/// drop it.
pub struct Agents {
    pub listings: mpsc::UnboundedReceiver<Listing>,
    tasks: BTreeMap<String, Task>,
}

struct Task {
    opening: oneshot::Sender<Opening>,
    done: JoinHandle<Result<(), Error>>,
}

impl Agents {
    /// Start every agent in `config`. Must be called inside a `LocalSet`.
    pub fn start(config: &Config, cwd: &Path) -> Agents {
        let (listings_tx, listings) = mpsc::unbounded_channel();
        let tasks = config
            .agents
            .iter()
            .map(|(name, agent)| {
                let (opening, waiting) = oneshot::channel();
                let done = spawn_local(connect(
                    name.clone(),
                    AcpAgent::new(agent.clone()),
                    cwd.to_path_buf(),
                    listings_tx.clone(),
                    waiting,
                ));
                (name.clone(), Task { opening, done })
            })
            .collect();
        Agents { listings, tasks }
    }

    /// Open `name` on `opening` and wait for it to finish. Every other
    /// agent is stopped. When `name` has already failed, this returns its
    /// error.
    pub async fn open(mut self, name: &str, opening: Opening) -> Result<(), Error> {
        let Some(chosen) = self.tasks.remove(name) else {
            return Ok(());
        };
        self.tasks.clear();
        let _ = chosen.opening.send(opening);
        chosen.done.await.unwrap_or(Ok(()))
    }
}

async fn connect(
    name: String,
    agent: AcpAgent,
    cwd: PathBuf,
    listings: mpsc::UnboundedSender<Listing>,
    opening: oneshot::Receiver<Opening>,
) -> Result<(), Error> {
    let result = acp::run(agent, async |link: Link| {
        let init = match link.initialize().await {
            Ok(init) => init,
            Err(error) => {
                none_listed(&listings, &name);
                return Err(error);
            }
        };
        let sessions = if can_list_and_load(&init) {
            link.list_sessions(cwd).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let _ = listings.send(Listing {
            agent: name.clone(),
            sessions,
        });
        match opening.await {
            Ok(opening) => app::run(link, init, Some(name.clone()), opening).await,
            Err(_) => Ok(()),
        }
    })
    .await;
    if result.is_err() {
        none_listed(&listings, &name);
    }
    result
}

fn none_listed(listings: &mpsc::UnboundedSender<Listing>, name: &str) {
    let _ = listings.send(Listing {
        agent: name.to_string(),
        sessions: Vec::new(),
    });
}

fn can_list_and_load(init: &InitializeResponse) -> bool {
    init.agent_capabilities.load_session
        && init.agent_capabilities.session_capabilities.list.is_some()
}
