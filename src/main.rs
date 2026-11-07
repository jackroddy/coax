use agent_client_protocol::AcpAgent;
use chat_tui::{acp, app};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: chat-tui <agent command> [args...]");
        std::process::exit(2);
    }
    let agent = match AcpAgent::from_args(args) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("chat-tui: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = acp::run(agent, app::run).await {
        ratatui::restore();
        eprintln!("chat-tui: {error}");
        std::process::exit(1);
    }
}
