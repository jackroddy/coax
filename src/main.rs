use agent_client_protocol::AcpAgent;
use coax::{acp, app};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: coax <agent command> [args...]");
        std::process::exit(2);
    }
    let agent = match AcpAgent::from_args(args) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("coax: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = acp::run(agent, app::run).await {
        ratatui::restore();
        eprintln!("coax: {error}");
        std::process::exit(1);
    }
}
